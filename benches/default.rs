use criterion::criterion_main;

mod batch;
mod event;
mod files;
mod http;
mod lua;
mod lua_data;
mod metrics_snapshot;
mod regex;
mod template;
mod topology;

criterion_main!(
    batch::benches,
    event::benches,
    files::benches,
    http::benches,
    lua::benches,
    lua_data::benches,
    metrics_snapshot::benches,
    regex::benches,
    template::benches,
    topology::benches,
);
