use serde::{Deserialize, Serialize};

use crate::{
    conditions::{Condition, ConditionConfig, ConditionDescription},
    Event,
};

//------------------------------------------------------------------------------

#[derive(Deserialize, Serialize, Debug, Default, Clone)]
pub struct IsLogConfig {}

inventory::submit! {
    ConditionDescription::new::<IsLogConfig>("is_log")
}

impl_generate_config_from_default!(IsLogConfig);

#[typetag::serde(name = "is_log")]
impl ConditionConfig for IsLogConfig {
    fn build(&self) -> crate::Result<Box<dyn Condition>> {
        Ok(Box::new(IsLog {}))
    }
}

//------------------------------------------------------------------------------

#[derive(Clone)]
pub struct IsLog {}

impl Condition for IsLog {
    fn check(&self, e: &Event) -> bool {
        matches!(e, Event::Log(_))
    }

    fn check_with_context(&self, e: &Event) -> Result<(), String> {
        if self.check(e) {
            Ok(())
        } else {
            Err("event is not a log type".to_string())
        }
    }
}

//------------------------------------------------------------------------------

#[cfg(test)]
mod test {
    use super::*;
    use crate::{
        event::metric::{Metric, MetricKind, MetricValue},
        log_event, Event,
    };

    #[test]
    fn generate_config() {
        crate::test_util::test_generate_config::<IsLogConfig>();
    }

    #[test]
    fn is_log_basic() {
        let cond = IsLogConfig {}.build().unwrap();

        assert_eq!(
            cond.check(&log_event! {
                crate::config::log_schema().message_key().clone() => "just a log".to_string(),
                crate::config::log_schema().timestamp_key().clone() => chrono::Utc::now(),
            }),
            true
        );
        assert_eq!(
            cond.check(&Event::from(Metric {
                name: "test metric".to_string(),
                namespace: None,
                timestamp: None,
                tags: None,
                kind: MetricKind::Incremental,
                value: MetricValue::Counter { value: 1.0 },
            })),
            false
        );
    }
}
