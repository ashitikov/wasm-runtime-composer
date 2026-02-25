# wasm-runtime-composer

Runtime composition of [WebAssembly components](https://component-model.bytecodealliance.org/) using [wasmtime](https://wasmtime.dev/).

## What it does

Takes multiple independently compiled WASM components, matches their imports to exports at runtime, and wires them into a single composition — no ahead-of-time tooling required.

- Automatic import/export resolution
- Resource proxying across component boundaries (own + borrow)
- Async support with concurrent calls on a single store

## Why

The WebAssembly Component Model defines how components import and export typed interfaces. Tools like [`wac`](https://github.com/bytecodealliance/wac) solve this at build time by producing a single fused component. But sometimes you need to compose at runtime:

- Components are loaded dynamically (plugins, user-uploaded modules)
- The set of components isn't known until the application starts
- You want to swap components without recompilation
- Different deployments use different component combinations

This crate provides the runtime equivalent: give it N components, and it figures out the wiring.

## How it works

Each component runs in its own wasmtime `Store` with a dedicated async inbox loop. Cross-component calls go through internal channels — no shared mutable state, no `Arc<Mutex>`. Resources (handles) that cross boundaries are transparently proxied so each store sees its own handle while the underlying resource lives in the producer's store.

```
+------------------------------------------------------+
|                     Composition                      |
|                                                      |
|  +-----------------+          +-----------------+    |
|  |   Producer      |          |   Consumer      |    |
|  |                 |          |                 |    |
|  |  Store A        |          |  Store B        |    |
|  |  +-----------+  |          |  +-----------+  |    |
|  |  |inbox loop |<---channel---->|inbox loop |  |    |
|  |  +-----------+  |          |  +-----------+  |    |
|  |                 |          |                 |    |
|  |  exports:       |          |  imports:       |    |
|  |    add()        |          |    add()        |    |
|  |    ping()       |          |    ping()       |    |
|  +-----------------+          +-----------------+    |
+------------------------------------------------------+
```

## Usage

### From uninstantiated components (recommended)

```rust
use wasmtime::{Config, Engine, Store};
use wasmtime::component::{Component, Linker, Val};
use wasm_runtime_composer::*;

#[tokio::main]
async fn main() -> Result<(), CompositionError> {
    let mut config = Config::new();
    config.wasm_component_model(true);
    config.wasm_component_model_async(true);
    let engine = Engine::new(&config).unwrap();

    // Load components
    let producer_wasm = Component::from_file(&engine, "producer.wasm").unwrap();
    let consumer_wasm = Component::from_file(&engine, "consumer.wasm").unwrap();

    // Create composables with their own linkers
    let mut producer_linker: Linker<MyState> = Linker::new(&engine);
    // ... add WASI or other host imports to the linker ...
    let producer = ComposableComponent::new(
        producer_wasm,
        producer_linker,
        move || Store::new(&engine, MyState::new()),
    );

    let mut consumer_linker: Linker<MyState> = Linker::new(&engine);
    // ... add host imports ...
    let consumer = ComposableComponent::new(
        consumer_wasm,
        consumer_linker,
        move || Store::new(&engine, MyState::new()),
    );

    // Compose — imports are resolved automatically
    let mut composer = Composer::new();
    composer.add(ComposableDescriptor::new("producer", producer));
    composer.add(ComposableDescriptor::new("consumer", consumer));
    let composition = composer.compose().await?;

    // Call an exported function
    let func = composition.get_func(None, "run-add")?;
    let mut results = vec![Val::S32(0)];
    func.call(&[], &mut results).await?;

    Ok(())
}
```

### From pre-instantiated components

If you already have a `Store` and `Instance`, wrap them directly:

```rust
let instance = linker.instantiate_async(&mut store, &component).await?;
let composable = ComposableInstance::from_existing(instance, store);
```

### Filtering exports

Control which interfaces a composable exposes. Useful for restricting access to specific interfaces or functions — for example, hiding internal APIs from untrusted components:

```rust
use wasm_runtime_composer::ExportFilter;

// Only expose specific exports
let filtered = producer.exposing(&["add", "composer:test/iproducer"]);

// Or hide specific exports
let filtered = producer.hiding(&["internal-debug"]);
```

### Disambiguation with hints

When multiple composables export the same interface, use hints:

```rust
let mut composer = Composer::new();
composer.add(
    ComposableDescriptor::new("consumer", consumer)
        .with_import_hint("add", "producer-a") // use producer-a for "add"
);
composer.with_export_hint("add", "producer-b"); // expose producer-b's "add"
```

## Features

- **`component-model-async`** (default) — enables `func_new_concurrent` for async WIT functions, allowing multiple concurrent calls on a single store via wasmtime's `Accessor<T>`

## Requirements

- Rust 2024 edition
- tokio runtime (components use async channels internally)
- wasmtime with component model support

## Roadmap

- **wRPC integration** — include remote components in a composition via [wRPC](https://github.com/wrpc/wrpc)
- **`stream` and `future` support** — wire component-model async types (`stream<T>`, `future<T>`) across component boundaries

## License

MIT
