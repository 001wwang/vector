use crate::{
    conditions::{AnyCondition, Condition},
    config::{DataType, TransformConfig, TransformDescription},
    event::discriminant::Discriminant,
    event::{Event, LogEvent, LookupBuf},
    internal_events::{ReduceEventProcessed, ReduceStaleEventFlushed},
    transforms::{TaskTransform, Transform},
};
use async_stream::stream;
use futures::{
    compat::{Compat, Compat01As03},
    stream, StreamExt,
};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::{hash_map, HashMap};
use std::time::{Duration, Instant};

mod merge_strategy;

use merge_strategy::*;

//------------------------------------------------------------------------------

#[derive(Deserialize, Serialize, Debug, Default, Clone)]
#[serde(deny_unknown_fields, default)]
pub struct ReduceConfig {
    pub expire_after_ms: Option<u64>,

    pub flush_period_ms: Option<u64>,

    /// An ordered list of fields to distinguish reduces by. Each
    /// reduce has a separate event merging state.
    #[serde(default)]
    pub group_by: Vec<LookupBuf>,

    #[serde(default)]
    pub merge_strategies: IndexMap<LookupBuf, MergeStrategy>,

    /// An optional condition that determines when an event is the end of a
    /// reduce.
    pub ends_when: Option<AnyCondition>,
    pub starts_when: Option<AnyCondition>,
}

inventory::submit! {
    TransformDescription::new::<ReduceConfig>("reduce")
}

impl_generate_config_from_default!(ReduceConfig);

#[async_trait::async_trait]
#[typetag::serde(name = "reduce")]
impl TransformConfig for ReduceConfig {
    async fn build(&self) -> crate::Result<Transform> {
        Reduce::new(self).map(Transform::task)
    }

    fn input_type(&self) -> DataType {
        DataType::Log
    }

    fn output_type(&self) -> DataType {
        DataType::Log
    }

    fn transform_type(&self) -> &'static str {
        "reduce"
    }
}

#[derive(Debug)]
struct ReduceState {
    fields: HashMap<LookupBuf, Box<dyn ReduceValueMerger>>,
    stale_since: Instant,
}

impl ReduceState {
    fn new(e: LogEvent, strategies: &IndexMap<LookupBuf, MergeStrategy>) -> Self {
        Self {
            stale_since: Instant::now(),
            fields: e
                .into_iter()
                .filter_map(|(k, v)| {
                    let k_lookup = LookupBuf::from_str(&k).unwrap_or_else(|_| LookupBuf::from(k));
                    if let Some(strat) = strategies.get(&k_lookup) {
                        match get_value_merger(v, strat) {
                            Ok(m) => Some((k_lookup, m)),
                            Err(error) => {
                                warn!(message = "Failed to create merger.", field = ?k_lookup, %error);
                                None
                            }
                        }
                    } else {
                        Some((k_lookup, v.into()))
                    }
                })
                .collect(),
        }
    }

    fn add_event(&mut self, e: LogEvent, strategies: &IndexMap<LookupBuf, MergeStrategy>) {
        for (k, v) in e.into_iter() {
            let k_lookup = LookupBuf::from(k);
            let strategy = strategies.get(&k_lookup);
            match self.fields.entry(k_lookup) {
                hash_map::Entry::Vacant(entry) => {
                    if let Some(strat) = strategy {
                        match get_value_merger(v, strat) {
                            Ok(m) => {
                                entry.insert(m);
                            }
                            Err(error) => {
                                warn!(message = "Failed to merge value.", %error);
                            }
                        }
                    } else {
                        entry.insert(v.clone().into());
                    }
                }
                hash_map::Entry::Occupied(mut entry) => {
                    if let Err(error) = entry.get_mut().add(v.clone()) {
                        warn!(message = "Failed to merge value.", %error);
                    }
                }
            }
        }
        self.stale_since = Instant::now();
    }

    fn flush(mut self) -> LogEvent {
        let mut event = Event::new_empty_log().into_log();
        for (k, v) in self.fields.drain() {
            if let Err(error) = v.insert_into(k, &mut event) {
                warn!(message = "Failed to merge values for field.", %error);
            }
        }
        event
    }
}

//------------------------------------------------------------------------------

pub struct Reduce {
    expire_after: Duration,
    flush_period: Duration,
    group_by: Vec<LookupBuf>,
    merge_strategies: IndexMap<LookupBuf, MergeStrategy>,
    reduce_merge_states: HashMap<Discriminant, ReduceState>,
    ends_when: Option<Box<dyn Condition>>,
    starts_when: Option<Box<dyn Condition>>,
}

impl Reduce {
    fn new(config: &ReduceConfig) -> crate::Result<Self> {
        if config.ends_when.is_some() && config.starts_when.is_some() {
            return Err("only one of `ends_when` and `starts_when` can be provided".into());
        }

        let ends_when = config.ends_when.as_ref().map(|c| c.build()).transpose()?;
        let starts_when = config.starts_when.as_ref().map(|c| c.build()).transpose()?;
        let group_by = config.group_by.clone().into_iter().collect();

        Ok(Reduce {
            expire_after: Duration::from_millis(config.expire_after_ms.unwrap_or(30000)),
            flush_period: Duration::from_millis(config.flush_period_ms.unwrap_or(1000)),
            group_by,
            merge_strategies: config.merge_strategies.clone(),
            reduce_merge_states: HashMap::new(),
            ends_when,
            starts_when,
        })
    }

    fn flush_into(&mut self, output: &mut Vec<Event>) {
        let mut flush_discriminants = Vec::new();
        for (k, t) in &self.reduce_merge_states {
            if t.stale_since.elapsed() >= self.expire_after {
                flush_discriminants.push(k.clone());
            }
        }
        for k in &flush_discriminants {
            if let Some(t) = self.reduce_merge_states.remove(k) {
                emit!(ReduceStaleEventFlushed);
                output.push(Event::from(t.flush()));
            }
        }
    }

    fn flush_all_into(&mut self, output: &mut Vec<Event>) {
        self.reduce_merge_states
            .drain()
            .for_each(|(_, s)| output.push(Event::from(s.flush())));
    }

    fn push_or_new_reduce_state(&mut self, event: LogEvent, discriminant: Discriminant) {
        match self.reduce_merge_states.entry(discriminant) {
            hash_map::Entry::Vacant(entry) => {
                entry.insert(ReduceState::new(event, &self.merge_strategies));
            }
            hash_map::Entry::Occupied(mut entry) => {
                entry.get_mut().add_event(event, &self.merge_strategies);
            }
        }
    }

    fn transform_one(&mut self, output: &mut Vec<Event>, event: Event) {
        let starts_here = self
            .starts_when
            .as_ref()
            .map(|c| c.check(&event))
            .unwrap_or(false);
        let ends_here = self
            .ends_when
            .as_ref()
            .map(|c| c.check(&event))
            .unwrap_or(false);

        let event = event.into_log();
        let discriminant = Discriminant::from_log_event(&event, &self.group_by);

        if starts_here {
            if let Some(state) = self.reduce_merge_states.remove(&discriminant) {
                output.push(state.flush().into());
            }

            self.push_or_new_reduce_state(event, discriminant)
        } else if ends_here {
            output.push(match self.reduce_merge_states.remove(&discriminant) {
                Some(mut state) => {
                    state.add_event(event, &self.merge_strategies);
                    state.flush().into()
                }
                None => ReduceState::new(event, &self.merge_strategies)
                    .flush()
                    .into(),
            })
        } else {
            self.push_or_new_reduce_state(event, discriminant)
        }

        emit!(ReduceEventProcessed);

        self.flush_into(output);
    }
}

impl TaskTransform for Reduce {
    fn transform(
        self: Box<Self>,
        input_rx: Box<dyn futures01::Stream<Item = Event, Error = ()> + Send>,
    ) -> Box<dyn futures01::Stream<Item = Event, Error = ()> + Send>
    where
        Self: 'static,
    {
        let mut me = self;

        let poll_period = me.flush_period;

        let mut flush_stream = tokio::time::interval(poll_period);
        let mut input_stream = Compat01As03::new(input_rx);

        let stream = stream! {
          loop {
            let mut output = Vec::new();
            let done = tokio::select! {
                _ = flush_stream.next() => {
                  me.flush_into(&mut output);
                  false
                }
                maybe_event = input_stream.next() => {
                  match maybe_event {
                    None => {
                      me.flush_all_into(&mut output);
                      true
                    }
                    Some(Ok(event)) => {
                      me.transform_one(&mut output, event);
                      false
                    }
                    Some(Err(())) => panic!("Unexpected error reading channel"),
                  }
                }
            };
            yield stream::iter(output.into_iter());
            if done { break }
          }
        }
        .flatten();

        // Needed for compat
        let try_stream = Box::pin(stream.map::<Result<Event, ()>, _>(Ok));

        Box::new(Compat::new(try_stream))
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{
        config::{log_schema, TransformConfig},
        event::{Lookup, Value},
        log_event,
    };
    use futures::compat::Stream01CompatExt;
    use serde_json::json;

    #[test]
    fn generate_config() {
        crate::test_util::test_generate_config::<ReduceConfig>();
    }

    #[tokio::test]
    async fn reduce_from_condition() {
        let reduce = toml::from_str::<ReduceConfig>(
            r#"
group_by = [ "request_id" ]

[ends_when]
  "test_end.exists" = true
"#,
        )
        .unwrap()
        .build()
        .await
        .unwrap();
        let reduce = reduce.into_task();

        let mut e_1 = log_event! {
            log_schema().message_key().clone() => "test message 1".to_string(),
            log_schema().timestamp_key().clone() => chrono::Utc::now(),
        };
        e_1.as_mut_log().insert(LookupBuf::from("counter"), 1);
        e_1.as_mut_log().insert(LookupBuf::from("request_id"), "1");

        let mut e_2 = log_event! {
            log_schema().message_key().clone() => "test message 2".to_string(),
            log_schema().timestamp_key().clone() => chrono::Utc::now(),
        };
        e_2.as_mut_log().insert(LookupBuf::from("counter"), 2);
        e_2.as_mut_log().insert(LookupBuf::from("request_id"), "2");

        let mut e_3 = log_event! {
            log_schema().message_key().clone() => "test message 3".to_string(),
            log_schema().timestamp_key().clone() => chrono::Utc::now(),
        };
        e_3.as_mut_log().insert(LookupBuf::from("counter"), 3);
        e_3.as_mut_log().insert(LookupBuf::from("request_id"), "1");

        let mut e_4 = log_event! {
            log_schema().message_key().clone() => "test message 4".to_string(),
            log_schema().timestamp_key().clone() => chrono::Utc::now(),
        };
        e_4.as_mut_log().insert(LookupBuf::from("counter"), 4);
        e_4.as_mut_log().insert(LookupBuf::from("request_id"), "1");
        e_4.as_mut_log().insert(LookupBuf::from("test_end"), "yep");

        let mut e_5 = log_event! {
            log_schema().message_key().clone() => "test message 5".to_string(),
            log_schema().timestamp_key().clone() => chrono::Utc::now(),
        };
        e_5.as_mut_log().insert(LookupBuf::from("counter"), 5);
        e_5.as_mut_log().insert(LookupBuf::from("request_id"), "2");
        e_5.as_mut_log()
            .insert(LookupBuf::from("extra_field"), "value1");
        e_5.as_mut_log().insert(LookupBuf::from("test_end"), "yep");

        let inputs = vec![e_1, e_2, e_3, e_4, e_5];
        let in_stream = futures01::stream::iter_ok(inputs);
        let mut out_stream = reduce.transform(Box::new(in_stream)).compat();

        let output_1 = out_stream.next().await.unwrap().unwrap();
        assert_eq!(
            output_1.as_log()[Lookup::from("message")],
            "test message 1".into()
        );
        assert_eq!(output_1.as_log()[Lookup::from("counter")], Value::from(8));

        let output_2 = out_stream.next().await.unwrap().unwrap();
        assert_eq!(
            output_2.as_log()[Lookup::from("message")],
            "test message 2".into()
        );
        assert_eq!(
            output_2.as_log()[Lookup::from("extra_field")],
            "value1".into()
        );
        assert_eq!(output_2.as_log()[Lookup::from("counter")], Value::from(7));
    }

    #[tokio::test]
    async fn reduce_merge_strategies() {
        let reduce = toml::from_str::<ReduceConfig>(
            r#"
group_by = [ "request_id" ]

merge_strategies.foo = "concat"
merge_strategies.bar = "array"
merge_strategies.baz = "max"

[ends_when]
  "test_end.exists" = true
"#,
        )
        .unwrap()
        .build()
        .await
        .unwrap();
        let reduce = reduce.into_task();

        let mut e_1 = log_event! {
            log_schema().message_key().clone() => "test message 1".to_string(),
            log_schema().timestamp_key().clone() => chrono::Utc::now(),
        };
        e_1.as_mut_log().insert(LookupBuf::from("foo"), "first foo");
        e_1.as_mut_log().insert(LookupBuf::from("bar"), "first bar");
        e_1.as_mut_log().insert(LookupBuf::from("baz"), 2);
        e_1.as_mut_log().insert(LookupBuf::from("request_id"), "1");

        let mut e_2 = log_event! {
            log_schema().message_key().clone() => "test message 2".to_string(),
            log_schema().timestamp_key().clone() => chrono::Utc::now(),
        };
        e_2.as_mut_log()
            .insert(LookupBuf::from("foo"), "second foo");
        e_2.as_mut_log().insert(LookupBuf::from("bar"), 2);
        e_2.as_mut_log()
            .insert(LookupBuf::from("baz"), "not number");
        e_2.as_mut_log().insert(LookupBuf::from("request_id"), "1");

        let mut e_3 = log_event! {
            log_schema().message_key().clone() => "test message 3".to_string(),
            log_schema().timestamp_key().clone() => chrono::Utc::now(),
        };
        e_3.as_mut_log().insert(LookupBuf::from("foo"), 10);
        e_3.as_mut_log().insert(LookupBuf::from("bar"), "third bar");
        e_3.as_mut_log().insert(LookupBuf::from("baz"), 3);
        e_3.as_mut_log().insert(LookupBuf::from("request_id"), "1");
        e_3.as_mut_log().insert(LookupBuf::from("test_end"), "yep");

        let inputs = vec![e_1, e_2, e_3];
        let in_stream = futures01::stream::iter_ok(inputs);
        let mut out_stream = reduce.transform(Box::new(in_stream)).compat();

        let output_1 = out_stream.next().await.unwrap().unwrap();
        assert_eq!(
            output_1.as_log()[Lookup::from("message")],
            "test message 1".into()
        );
        assert_eq!(
            output_1.as_log()[Lookup::from("foo")],
            "first foo second foo".into()
        );
        assert_eq!(
            output_1.as_log()[Lookup::from("bar")],
            Value::Array(vec!["first bar".into(), 2.into(), "third bar".into()]),
        );
        assert_eq!(output_1.as_log()[Lookup::from("baz")], 3.into(),);
    }

    #[tokio::test]
    async fn missing_group_by() {
        let reduce = toml::from_str::<ReduceConfig>(
            r#"
group_by = [ "request_id" ]

[ends_when]
  "test_end.exists" = true
"#,
        )
        .unwrap()
        .build()
        .await
        .unwrap();
        let reduce = reduce.into_task();

        let mut e_1 = log_event! {
            log_schema().message_key().clone() => "test message 1".to_string(),
            log_schema().timestamp_key().clone() => chrono::Utc::now(),
        };
        e_1.as_mut_log().insert(LookupBuf::from("counter"), 1);
        e_1.as_mut_log().insert(LookupBuf::from("request_id"), "1");

        let mut e_2 = log_event! {
            log_schema().message_key().clone() => "test message 2".to_string(),
            log_schema().timestamp_key().clone() => chrono::Utc::now(),
        };
        e_2.as_mut_log().insert(LookupBuf::from("counter"), 2);

        let mut e_3 = log_event! {
            log_schema().message_key().clone() => "test message 3".to_string(),
            log_schema().timestamp_key().clone() => chrono::Utc::now(),
        };
        e_3.as_mut_log().insert(LookupBuf::from("counter"), 3);
        e_3.as_mut_log().insert(LookupBuf::from("request_id"), "1");

        let mut e_4 = log_event! {
            log_schema().message_key().clone() => "test message 4".to_string(),
            log_schema().timestamp_key().clone() => chrono::Utc::now(),
        };
        e_4.as_mut_log().insert(LookupBuf::from("counter"), 4);
        e_4.as_mut_log().insert(LookupBuf::from("request_id"), "1");
        e_4.as_mut_log().insert(LookupBuf::from("test_end"), "yep");

        let mut e_5 = log_event! {
            log_schema().message_key().clone() => "test message 5".to_string(),
            log_schema().timestamp_key().clone() => chrono::Utc::now(),
        };
        e_5.as_mut_log().insert(LookupBuf::from("counter"), 5);
        e_5.as_mut_log()
            .insert(LookupBuf::from("extra_field"), "value1");
        e_5.as_mut_log().insert(LookupBuf::from("test_end"), "yep");

        let inputs = vec![e_1, e_2, e_3, e_4, e_5];
        let in_stream = Box::new(futures01::stream::iter_ok(inputs));
        let mut out_stream = reduce.transform(in_stream).compat();

        let output_1 = out_stream.next().await.unwrap().unwrap();
        let output_1 = output_1.as_log();
        assert_eq!(output_1[Lookup::from("message")], "test message 1".into());
        assert_eq!(output_1[Lookup::from("counter")], Value::from(8));

        let output_2 = out_stream.next().await.unwrap().unwrap();
        let output_2 = output_2.as_log();
        assert_eq!(output_2[Lookup::from("message")], "test message 2".into());
        assert_eq!(output_2[Lookup::from("extra_field")], "value1".into());
        assert_eq!(output_2[Lookup::from("counter")], Value::from(7));
    }

    #[tokio::test]
    async fn arrays() {
        let reduce = toml::from_str::<ReduceConfig>(
            r#"
group_by = [ "request_id" ]

merge_strategies.foo = "array"
merge_strategies.bar = "concat"

[ends_when]
  "test_end.exists" = true
"#,
        )
        .unwrap()
        .build()
        .await
        .unwrap();
        let reduce = reduce.into_task();

        let mut e_1 = log_event! {
            log_schema().message_key().clone() => "test message 1".to_string(),
            log_schema().timestamp_key().clone() => chrono::Utc::now(),
        };
        e_1.as_mut_log()
            .insert(LookupBuf::from("foo"), json!([1, 3]));
        e_1.as_mut_log()
            .insert(LookupBuf::from("bar"), json!([1, 3]));
        e_1.as_mut_log().insert(LookupBuf::from("request_id"), "1");

        let mut e_2 = log_event! {
            log_schema().message_key().clone() => "test message 2".to_string(),
            log_schema().timestamp_key().clone() => chrono::Utc::now(),
        };
        e_2.as_mut_log()
            .insert(LookupBuf::from("foo"), json!([2, 4]));
        e_2.as_mut_log()
            .insert(LookupBuf::from("bar"), json!([2, 4]));
        e_2.as_mut_log().insert(LookupBuf::from("request_id"), "2");

        let mut e_3 = log_event! {
            log_schema().message_key().clone() => "test message 3".to_string(),
            log_schema().timestamp_key().clone() => chrono::Utc::now(),
        };
        e_3.as_mut_log()
            .insert(LookupBuf::from("foo"), json!([5, 7]));
        e_3.as_mut_log()
            .insert(LookupBuf::from("bar"), json!([5, 7]));
        e_3.as_mut_log().insert(LookupBuf::from("request_id"), "1");

        let mut e_4 = log_event! {
            log_schema().message_key().clone() => "test message 4".to_string(),
            log_schema().timestamp_key().clone() => chrono::Utc::now(),
        };
        e_4.as_mut_log()
            .insert(LookupBuf::from("foo"), json!("done"));
        e_4.as_mut_log()
            .insert(LookupBuf::from("bar"), json!("done"));
        e_4.as_mut_log().insert(LookupBuf::from("request_id"), "1");
        e_4.as_mut_log().insert(LookupBuf::from("test_end"), "yep");

        let mut e_5 = log_event! {
            log_schema().message_key().clone() => "test message 5".to_string(),
            log_schema().timestamp_key().clone() => chrono::Utc::now(),
        };
        e_5.as_mut_log()
            .insert(LookupBuf::from("foo"), json!([6, 8]));
        e_5.as_mut_log()
            .insert(LookupBuf::from("bar"), json!([6, 8]));
        e_5.as_mut_log().insert(LookupBuf::from("request_id"), "2");

        let mut e_6 = log_event! {
            log_schema().message_key().clone() => "test message 6".to_string(),
            log_schema().timestamp_key().clone() => chrono::Utc::now(),
        };
        e_6.as_mut_log()
            .insert(LookupBuf::from("foo"), json!("done"));
        e_6.as_mut_log()
            .insert(LookupBuf::from("bar"), json!("done"));
        e_6.as_mut_log().insert(LookupBuf::from("request_id"), "2");
        e_6.as_mut_log().insert(LookupBuf::from("test_end"), "yep");

        let inputs = vec![e_1, e_2, e_3, e_4, e_5, e_6];
        let in_stream = Box::new(futures01::stream::iter_ok(inputs));
        let mut out_stream = reduce.transform(in_stream).compat();

        let output_1 = out_stream.next().await.unwrap().unwrap();
        let output_1 = output_1.as_log();
        assert_eq!(
            output_1[Lookup::from("foo")],
            json!([[1, 3], [5, 7], "done"]).into()
        );

        assert_eq!(
            output_1[Lookup::from("bar")],
            json!([1, 3, 5, 7, "done"]).into()
        );

        let output_1 = out_stream.next().await.unwrap().unwrap();
        let output_1 = output_1.as_log();
        assert_eq!(
            output_1[Lookup::from("foo")],
            json!([[2, 4], [6, 8], "done"]).into()
        );
        assert_eq!(
            output_1[Lookup::from("bar")],
            json!([2, 4, 6, 8, "done"]).into()
        );
    }
}
