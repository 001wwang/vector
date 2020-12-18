use crate::{
    config::{log_schema, DataType, GlobalOptions, Resource, SourceConfig, SourceDescription},
    event::{Event, LookupBuf},
    internal_events::{StdinEventReceived, StdinReadFailed},
    log_event,
    shutdown::ShutdownSignal,
    Pipeline,
};
use bytes::Bytes;
use futures::{executor, FutureExt, SinkExt, StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};
use std::{io, thread};
use tokio::sync::mpsc::channel;

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(deny_unknown_fields, default)]
pub struct StdinConfig {
    #[serde(default = "default_max_length")]
    pub max_length: usize,
    pub host_key: Option<LookupBuf>,
}

impl Default for StdinConfig {
    fn default() -> Self {
        StdinConfig {
            max_length: default_max_length(),
            host_key: None,
        }
    }
}

fn default_max_length() -> usize {
    bytesize::kib(100u64) as usize
}

inventory::submit! {
    SourceDescription::new::<StdinConfig>("stdin")
}

impl_generate_config_from_default!(StdinConfig);

#[async_trait::async_trait]
#[typetag::serde(name = "stdin")]
impl SourceConfig for StdinConfig {
    async fn build(
        &self,
        _name: &str,
        _globals: &GlobalOptions,
        shutdown: ShutdownSignal,
        out: Pipeline,
    ) -> crate::Result<super::Source> {
        stdin_source(io::BufReader::new(io::stdin()), self.clone(), shutdown, out)
    }

    fn output_type(&self) -> DataType {
        DataType::Log
    }

    fn source_type(&self) -> &'static str {
        "stdin"
    }

    fn resources(&self) -> Vec<Resource> {
        vec![Resource::Stdin]
    }
}

pub fn stdin_source<R>(
    stdin: R,
    config: StdinConfig,
    shutdown: ShutdownSignal,
    out: Pipeline,
) -> crate::Result<super::Source>
where
    R: Send + io::BufRead + 'static,
{
    let host_key = config
        .host_key
        .unwrap_or_else(|| log_schema().host_key().clone());
    let hostname = crate::get_hostname().ok();

    let (mut sender, receiver) = channel(1024);

    // Start the background thread
    thread::spawn(move || {
        info!("Capturing STDIN.");

        for line in stdin.lines() {
            if executor::block_on(sender.send(line)).is_err() {
                // receiver has closed so we should shutdown
                return;
            }
        }
    });

    Ok(Box::pin(async move {
        let mut out =
            out.sink_map_err(|error| error!(message = "Unable to send event to out.", %error));

        let res = receiver
            .take_until(shutdown)
            .map_err(|error| emit!(StdinReadFailed { error }))
            .map_ok(move |line| {
                emit!(StdinEventReceived {
                    byte_size: line.len()
                });
                create_event(Bytes::from(line), host_key.clone(), hostname.clone())
            })
            .forward(&mut out)
            .inspect(|_| info!("Finished sending."))
            .await;

        let _ = out.flush().await; // error emitted by sink_map_err

        res
    }))
}

fn create_event(line: Bytes, host_key: LookupBuf, hostname: Option<String>) -> Event {
    let mut event = log_event! {
        crate::config::log_schema().message_key().clone() => line,
        crate::config::log_schema().timestamp_key().clone() => chrono::Utc::now(),
    };

    // Add source type
    event
        .as_mut_log()
        .insert(log_schema().source_type_key().clone(), Bytes::from("stdin"));

    if let Some(hostname) = hostname {
        event.as_mut_log().insert(host_key, hostname);
    }

    event
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        event::{Lookup, LookupBuf},
        test_util::trace_init,
        Pipeline,
    };
    use std::io::Cursor;
    use tokio::sync::mpsc;

    #[test]
    fn generate_config() {
        crate::test_util::test_generate_config::<StdinConfig>();
    }

    #[test]
    fn stdin_create_event() {
        let line = Bytes::from("hello world");
        let host_key = LookupBuf::from("host");
        let hostname = Some("Some.Machine".to_string());

        let event = create_event(line, host_key, hostname);
        let log = event.into_log();

        assert_eq!(log[Lookup::from("host")], "Some.Machine".into());
        assert_eq!(log[log_schema().message_key()], "hello world".into());
        assert_eq!(log[log_schema().source_type_key()], "stdin".into());
    }

    #[tokio::test]
    async fn stdin_decodes_line() {
        trace_init();

        let (tx, mut rx) = Pipeline::new_test();
        let config = StdinConfig::default();
        let buf = Cursor::new("hello world\nhello world again");

        stdin_source(buf, config, ShutdownSignal::noop(), tx)
            .unwrap()
            .await
            .unwrap();

        let event = rx.try_recv();

        assert!(event.is_ok());
        assert_eq!(
            Ok("hello world".into()),
            event.map(|event| event.as_log()[log_schema().message_key()].to_string_lossy())
        );

        let event = rx.try_recv();
        assert!(event.is_ok());
        assert_eq!(
            Ok("hello world again".into()),
            event.map(|event| event.as_log()[log_schema().message_key()].to_string_lossy())
        );

        let event = rx.try_recv();
        assert!(event.is_err());
        assert_eq!(Err(mpsc::error::TryRecvError::Closed), event);
    }
}
