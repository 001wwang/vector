use crate::{
    config::{log_schema, DataType, GenerateConfig, TransformConfig, TransformDescription},
    event::{self, Event, LogEvent, LookupBuf},
    internal_events::{MetricToLogEventProcessed, MetricToLogFailedSerialize},
    transforms::{FunctionTransform, Transform},
    types::Conversion,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Deserialize, Serialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct MetricToLogConfig {
    pub host_tag: Option<LookupBuf>,
}

inventory::submit! {
    TransformDescription::new::<MetricToLogConfig>("metric_to_log")
}

impl GenerateConfig for MetricToLogConfig {
    fn generate_config() -> toml::Value {
        toml::Value::try_from(Self {
            host_tag: Some(LookupBuf::from("host-tag")),
        })
        .unwrap()
    }
}

#[async_trait::async_trait]
#[typetag::serde(name = "metric_to_log")]
impl TransformConfig for MetricToLogConfig {
    async fn build(&self) -> crate::Result<Transform> {
        Ok(Transform::function(MetricToLog::new(self.host_tag.clone())))
    }

    fn input_type(&self) -> DataType {
        DataType::Metric
    }

    fn output_type(&self) -> DataType {
        DataType::Log
    }

    fn transform_type(&self) -> &'static str {
        "metric_to_log"
    }
}

#[derive(Clone, Debug)]
pub struct MetricToLog {
    timestamp_key: LookupBuf,
    host_tag: LookupBuf,
}

impl MetricToLog {
    pub fn new(host_tag: Option<LookupBuf>) -> Self {
        let host_tag = host_tag.unwrap_or_else(|| log_schema().host_key().clone());
        let mut tag_lookup = LookupBuf::from("tags");
        tag_lookup.extend(host_tag);
        Self {
            timestamp_key: "timestamp".into(),
            host_tag: tag_lookup,
        }
    }
}

impl FunctionTransform for MetricToLog {
    fn transform(&mut self, output: &mut Vec<Event>, event: Event) {
        let metric = event.into_metric();
        emit!(MetricToLogEventProcessed);

        let retval = serde_json::to_value(&metric)
            .map_err(|error| emit!(MetricToLogFailedSerialize { error }))
            .ok()
            .and_then(|value| match value {
                Value::Object(object) => {
                    let mut log = LogEvent::default();

                    for (key, value) in object {
                        log.insert(LookupBuf::from(key), value);
                    }

                    let timestamp = log
                        .remove(&self.timestamp_key, false)
                        .and_then(|value| Conversion::Timestamp.convert(value.into_bytes()).ok())
                        .unwrap_or_else(|| event::Value::Timestamp(Utc::now()));
                    log.insert(log_schema().timestamp_key().clone(), timestamp);

                    if let Some(host) = log.remove(&self.host_tag, true) {
                        log.insert(log_schema().host_key().clone(), host);
                    }

                    Some(log.into())
                }
                _ => None,
            });
        output.extend(retval.into_iter())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{
        metric::{MetricKind, MetricValue, StatisticKind},
        Lookup, Metric, Value,
    };
    use chrono::{offset::TimeZone, DateTime, Utc};
    use std::collections::BTreeMap;

    #[test]
    fn generate_config() {
        crate::test_util::test_generate_config::<MetricToLogConfig>();
    }

    fn do_transform(metric: Metric) -> Option<LogEvent> {
        let event = Event::Metric(metric);
        let mut transformer = MetricToLog::new(Some("host".into()));

        transformer
            .transform_one(event)
            .map(|event| event.into_log())
    }

    fn ts() -> DateTime<Utc> {
        Utc.ymd(2018, 11, 14).and_hms_nano(8, 9, 10, 11)
    }

    fn tags() -> BTreeMap<String, String> {
        vec![
            ("host".to_owned(), "localhost".to_owned()),
            ("some_tag".to_owned(), "some_value".to_owned()),
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn transform_counter() {
        let counter = Metric {
            name: "counter".into(),
            namespace: None,
            timestamp: Some(ts()),
            tags: Some(tags()),
            kind: MetricKind::Absolute,
            value: MetricValue::Counter { value: 1.0 },
        };

        let log = do_transform(counter).unwrap();
        let collected: Vec<_> = log.pairs(true).collect();

        assert_eq!(
            collected,
            vec![
                (
                    Lookup::from_str("counter.value").unwrap(),
                    &Value::from(1.0)
                ),
                (Lookup::from_str("host").unwrap(), &Value::from("localhost")),
                (Lookup::from_str("kind").unwrap(), &Value::from("absolute")),
                (Lookup::from_str("name").unwrap(), &Value::from("counter")),
                (
                    Lookup::from_str("tags.some_tag").unwrap(),
                    &Value::from("some_value")
                ),
                (Lookup::from_str("timestamp").unwrap(), &Value::from(ts())),
            ]
        );
    }

    #[test]
    fn transform_gauge() {
        let gauge = Metric {
            name: "gauge".into(),
            namespace: None,
            timestamp: Some(ts()),
            tags: None,
            kind: MetricKind::Absolute,
            value: MetricValue::Gauge { value: 1.0 },
        };

        let log = do_transform(gauge).unwrap();
        let collected: Vec<_> = log.pairs(true).collect();

        assert_eq!(
            collected,
            vec![
                (Lookup::from_str("gauge.value").unwrap(), &Value::from(1.0)),
                (Lookup::from_str("kind").unwrap(), &Value::from("absolute")),
                (Lookup::from_str("name").unwrap(), &Value::from("gauge")),
                (Lookup::from_str("timestamp").unwrap(), &Value::from(ts())),
            ]
        );
    }

    #[test]
    fn transform_set() {
        let set = Metric {
            name: "set".into(),
            namespace: None,
            timestamp: Some(ts()),
            tags: None,
            kind: MetricKind::Absolute,
            value: MetricValue::Set {
                values: vec!["one".into(), "two".into()].into_iter().collect(),
            },
        };

        let log = do_transform(set).unwrap();
        let collected: Vec<_> = log.pairs(true).collect();

        assert_eq!(
            collected,
            vec![
                (Lookup::from_str("kind").unwrap(), &Value::from("absolute")),
                (Lookup::from_str("name").unwrap(), &Value::from("set")),
                (
                    Lookup::from_str("set.values[0]").unwrap(),
                    &Value::from("one")
                ),
                (
                    Lookup::from_str("set.values[1]").unwrap(),
                    &Value::from("two")
                ),
                (Lookup::from_str("timestamp").unwrap(), &Value::from(ts())),
            ]
        );
    }

    #[test]
    fn transform_distribution() {
        let distro = Metric {
            name: "distro".into(),
            namespace: None,
            timestamp: Some(ts()),
            tags: None,
            kind: MetricKind::Absolute,
            value: MetricValue::Distribution {
                values: vec![1.0, 2.0],
                sample_rates: vec![10, 20],
                statistic: StatisticKind::Histogram,
            },
        };

        let log = do_transform(distro).unwrap();
        let collected: Vec<_> = log.pairs(true).collect();

        assert_eq!(
            collected,
            vec![
                (
                    Lookup::from_str("distribution.sample_rates[0]").unwrap(),
                    &Value::from(10)
                ),
                (
                    Lookup::from_str("distribution.sample_rates[1]").unwrap(),
                    &Value::from(20)
                ),
                (
                    Lookup::from_str("distribution.statistic").unwrap(),
                    &Value::from("histogram")
                ),
                (
                    Lookup::from_str("distribution.values[0]").unwrap(),
                    &Value::from(1.0)
                ),
                (
                    Lookup::from_str("distribution.values[1]").unwrap(),
                    &Value::from(2.0)
                ),
                (Lookup::from_str("kind").unwrap(), &Value::from("absolute")),
                (Lookup::from_str("name").unwrap(), &Value::from("distro")),
                (Lookup::from_str("timestamp").unwrap(), &Value::from(ts())),
            ]
        );
    }

    #[test]
    fn transform_histogram() {
        let histo = Metric {
            name: "histo".into(),
            namespace: None,
            timestamp: Some(ts()),
            tags: None,
            kind: MetricKind::Absolute,
            value: MetricValue::AggregatedHistogram {
                buckets: vec![1.0, 2.0],
                counts: vec![10, 20],
                count: 30,
                sum: 50.0,
            },
        };

        let log = do_transform(histo).unwrap();
        let collected: Vec<_> = log.pairs(true).collect();

        assert_eq!(
            collected,
            vec![
                (
                    Lookup::from_str("aggregated_histogram.buckets[0]").unwrap(),
                    &Value::from(1.0)
                ),
                (
                    Lookup::from_str("aggregated_histogram.buckets[1]").unwrap(),
                    &Value::from(2.0)
                ),
                (
                    Lookup::from_str("aggregated_histogram.count").unwrap(),
                    &Value::from(30)
                ),
                (
                    Lookup::from_str("aggregated_histogram.counts[0]").unwrap(),
                    &Value::from(10)
                ),
                (
                    Lookup::from_str("aggregated_histogram.counts[1]").unwrap(),
                    &Value::from(20)
                ),
                (
                    Lookup::from_str("aggregated_histogram.sum").unwrap(),
                    &Value::from(50.0)
                ),
                (Lookup::from_str("kind").unwrap(), &Value::from("absolute")),
                (Lookup::from_str("name").unwrap(), &Value::from("histo")),
                (Lookup::from_str("timestamp").unwrap(), &Value::from(ts())),
            ]
        );
    }

    #[test]
    fn transform_summary() {
        let summary = Metric {
            name: "summary".into(),
            namespace: None,
            timestamp: Some(ts()),
            tags: None,
            kind: MetricKind::Absolute,
            value: MetricValue::AggregatedSummary {
                quantiles: vec![50.0, 90.0],
                values: vec![10.0, 20.0],
                count: 30,
                sum: 50.0,
            },
        };

        let log = do_transform(summary).unwrap();
        let collected: Vec<_> = log.pairs(true).collect();

        assert_eq!(
            collected,
            vec![
                (
                    Lookup::from_str("aggregated_summary.count").unwrap(),
                    &Value::from(30)
                ),
                (
                    Lookup::from_str("aggregated_summary.quantiles[0]").unwrap(),
                    &Value::from(50.0)
                ),
                (
                    Lookup::from_str("aggregated_summary.quantiles[1]").unwrap(),
                    &Value::from(90.0)
                ),
                (
                    Lookup::from_str("aggregated_summary.sum").unwrap(),
                    &Value::from(50.0)
                ),
                (
                    Lookup::from_str("aggregated_summary.values[0]").unwrap(),
                    &Value::from(10.0)
                ),
                (
                    Lookup::from_str("aggregated_summary.values[1]").unwrap(),
                    &Value::from(20.0)
                ),
                (Lookup::from_str("kind").unwrap(), &Value::from("absolute")),
                (Lookup::from_str("name").unwrap(), &Value::from("summary")),
                (Lookup::from_str("timestamp").unwrap(), &Value::from(ts())),
            ]
        );
    }
}
