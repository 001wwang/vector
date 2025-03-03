---
title: Transformation
description: Use Vector to transform observability data
weight: 2
---

Vector provides multiple [transforms] that you can use to modify your observability data as it
passes between the [components] that comprise your Vector [topology].

The transform that you will likely use most often for modifying data is the [`remap`][remap]
transform, which enables you to use a single-purpose data transformation language called [Vector
Remap Language][vrl] (VRL for short) to define your event transformation logic. VRL has several
features that should make it your first choice for transforming data in Vector:

* It offers a wide range of observability-data-specific [functions] that map directly onto
  observability use cases (and a [wide range][examples] of use cases at that!).

* It's built for the very specific use case of working with Vector logs and metrics, which means
  that it has no extraneous functionality, its data model maps directly to Vector's internal data
  model, and its performance comes quite close to that of native [Rust].

* The VRL compiler that's built into Vector performs compile-time checks to ensure that your VRL
  code is fundamentally sound. It checks for dead code, unhandled errors, type mismatches, and more.

In cases where VRL doesn't fit your use case, Vector also offers two [runtime
transforms](#runtime-transforms) that offer a bit more flexibility than VRL but
also come with downsides (listed below) that should always be borne in mind.

{{< success title="Tell us about your non-covered use case!" >}}
If your observability use case isn't covered by VRL, please feel *very* welcome to [open an
issue][issue] describing it. The Vector team will follow up with potential solutions and workarounds
or, in some cases, updates to VRL that directly address your needs.

[issue]: https://github.com/timberio/vector/issues/new?assignees=&labels=type%3A+enhancement&template=enhancement.md&title=
{{< /success >}}

## Transforming data using VRL

Let's jump straight into an example of using VRL to modify some data. We'll
create a simple topology consisting of three components:

1. A [`generator`][generator] source produces random [Syslog][syslog] messages at a rate of 10 per
   second.

2. A [`remap`][remap] transform uses VRL to parse incoming Syslog lines
   into named fields (`severity`, `timestamp`, etc.).

3. A [`console`][console] sink pipes the output of the topology to stdout,
   so that we can see the results on the command line.

This configuration defines that topology:

```toml title="vector.toml"
[sources.logs]
  type = "generator"
  format = "syslog"
  interval = 0.1

[transforms.modify]
  type = "remap"
  inputs = ["logs"]
  source = '''
    # Parse Syslog input. The "!" means that the script should abort on error.
    . = parse_syslog!(.message)
  '''

[sinks.out]
  type = "console"
  inputs = ["modify"]
  encoding.codec = "json"
```

{{< success title="Multiple configuration formats supported" >}}
Although we're using [TOML] for our configuration here, Vector also supports [JSON] and [YAML].

[json]: https://json.org
[toml]: https://toml.io
[yaml]: https://yaml.org
{{< /success >}}

To start Vector using this topology:

```bash
vector --config-toml /etc/vector/vector.toml
```

You should see lines like this emitted via stdout (formatted for readability here):

```json
{
  "appname": "authsvc",
  "facility": "daemon",
  "hostname": "acmecorp.biz",
  "message": "#hugops to everyone who has to deal with this",
  "msgid": "ID486",
  "procid": 5265,
  "severity": "notice",
  "timestamp": "2021-01-19T18:16:40.027Z"
}
```

So far, we've gotten Vector to *parse* the Syslog data but we're not yet *modifying* that data. So
let's update the `source` script of our `remap` transform to make some ad hoc transformations:

```toml
[transforms.modify]
type = "remap"
inputs = ["logs"]
source = '''
  . = parse_syslog!(.message)

  # Convert the timestamp to a Unix timestamp, aborting on error
  .timestamp = to_unix_timestamp!(.timestamp)

  # Remove the "facility" and "procid" fields
  del(.facility); del(.procid)

  # Replace the "msgid" field with a unique ID
  .msgid = uuid_v4()

  # If the log message contains the phrase "Great Scott!", set the new field
  # "critical" to true, otherwise set it to false. If the "contains" function
  # errors, log the error (instead of aborting the script, as above).
  if (is_critical, err = contains(.message, "Great Scott!"); err != null) {
    log(err, level: "error")
  }

  .critical = is_critical
'''
```

A few things to notice about this script:

* Any errors thrown by VRL functions must be handled. Were we to neglect to handle the potential
  error thrown by the `parse_syslog` function, for example, the VRL compiler would provide a very
  specific warning and Vector wouldn't start up.
* VRL has language constructs like variables, `if` statements, comments, and logging.
* The `.` acts as a sort of "container" for the event data. `.` by itself refers  to the root event,
  while you can use [paths] like `.foo`, `.foo[0]`, `.foo.bar`, `.foo.bar[0]`, and so on to
  reference subfields, array indices, and more.

If you stop and restart Vector, you should see log lines like this (again
reformatted for readability):

```json
{
  "appname": "authsvc",
  "hostname": "acmecorp.biz",
  "message": "Great Scott! We're never gonna reach 88 mph with the flux capacitor in its current state!",
  "msgid": "4e4437b6-13e8-43b3-b51e-c37bd46de490",
  "severity": "notice",
  "timestamp": 1611080200,
  "critical": true
}
```

And that's it! We've successfully created a Vector topology that transforms every event that passes
through it. If you'd like to know more about VRL, we recommend checking out the following
documentation:

* A full list listing of [VRL functions][functions]
* [VRL examples][examples]
* The [VRL specification][spec], which describes things like VRL's syntax and type system in great
  detail

## Runtime transforms

If VRL doesn't cover your use case—and that should happen rarely—Vector also
offers two **runtime transforms** that you can use instead of VRL:

* The [`wasm`][wasm_transform] transform enables you to run compiled [WebAssembly][wasm] code using
  a Wasm runtime inside of Vector.

* The [`lua`][lua_transform] transform enables you to run [Lua][lua] code that you can include
  directly in your Vector configuration

Both of the runtime transforms provide maximal flexibility because they enable
you to use full-fledged programming languages right inside of Vector. But we
recommend using these transforms only when truly necessary, for several reasons:

1. The runtime transforms make it all too easy to write scripts that are slow,
   error prone, and hard to read.
2. Both require you to add a coding/testing/debugging workflow to using Vector,
   which is worth the effort if there's no other way to satisfy your use case
   but best avoided if possible.
3. Both impose a performance penalty vis-à-vis VRL. Wasm does tend to be faster
   than Lua, but Wasm is more difficult to use given the need to add a
   Wasm compilation step to your Vector workflow.

[components]: /components
[console]: /docs/reference/configuration/sinks/console
[examples]: /docs/reference/vrl/examples
[functions]: /docs/reference/vrl/functions
[generator]: /docs/reference/configuration/sources/generator
[lua]: https://lua.org
[lua_transform]: /docs/reference/configuration/transforms/lua
[paths]: /docs/reference/vrl/expressions/#path
[remap]: /docs/reference/configuration/transforms/remap
[rust]: https://rust-lang.org
[spec]: /docs/reference/vrl/expressions
[syslog]: https://en.wikipedia.org/wiki/Syslog
[toml]: https://toml.io
[topology]: /docs/setup/deployment/topologies
[transforms]: /transforms
[vrl]: /docs/reference/vrl
[wasm]: https://webassembly.org
[wasm_transform]: /docs/reference/configuration/transforms/wasm
