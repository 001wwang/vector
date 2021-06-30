use criterion::{criterion_group, BatchSize, BenchmarkId, Criterion, Throughput};
use futures::{SinkExt, StreamExt};
use indoc::indoc;
use vector::{
    event::{metric::Sample, Event, Metric, MetricKind, MetricValue, StatisticKind},
    transforms,
};
use vector_core::transform::TaskTransform;

fn bench_histograms(c: &mut Criterion) {
    let config = indoc! {r#"
        hooks.process = """
            function (event, emit)
                emit(event)
            end
        """
    "#};

    let transform =
        Box::new(transforms::lua::v2::Lua::new(&toml::from_str(config).unwrap()).unwrap())
            as Box<dyn TaskTransform>;
    let (tx, rx) = futures::channel::mpsc::channel::<Event>(1);
    let mut rx = transform.transform(Box::pin(rx));

    let mut group = c.benchmark_group("lua/histograms");
    group.throughput(Throughput::Elements(1));

    for &size in [0, 1, 10, 100, 1000, 10000].iter() {
        group.bench_with_input(BenchmarkId::new("size", size), &size, |b, &size| {
            let event = make_metric(size);
            b.iter_batched(
                || (tx.clone(), event.clone()),
                |(mut tx, event)| {
                    futures::executor::block_on(tx.send(event)).unwrap();
                    futures::executor::block_on(rx.next()).unwrap()
                },
                BatchSize::SmallInput,
            )
        });
    }
    group.finish();
}

fn make_metric(size: u32) -> Event {
    let samples: Vec<_> = (0..size)
        .map(|i| Sample {
            value: i as f64,
            rate: i,
        })
        .collect();
    Metric::new(
        "the metric",
        MetricKind::Absolute,
        MetricValue::Distribution {
            samples,
            statistic: StatisticKind::Histogram,
        },
    )
    .into()
}

criterion_group!(
    name = benches;
    // encapsulates CI noise we saw in
    // https://github.com/timberio/vector/issues/5394
    config = Criterion::default().noise_threshold(0.05);
    targets = bench_histograms
);
