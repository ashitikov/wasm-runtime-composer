use wasmtime::component::{Component, Linker, ResourceTable, Val};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use wasm_runtime_composer::composable_instance::ComposableInstance;
use wasm_runtime_composer::{Composable, ComposableDescriptor, CompositionBuilder};

struct TestState {
    ctx: WasiCtx,
    table: ResourceTable,
}

impl TestState {
    fn new() -> Self {
        Self {
            ctx: WasiCtxBuilder::new().build(),
            table: ResourceTable::new(),
        }
    }
}

impl WasiView for TestState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.ctx,
            table: &mut self.table,
        }
    }
}

fn guests_dir() -> String {
    format!(
        "{}/tests/guests/target/wasm32-wasip2/release",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// Helper: instantiate producer and consumer, wire them up, return composition.
async fn make_composition(engine: &Engine) -> wasm_runtime_composer::Composition {
    let dir = guests_dir();

    let producer_component =
        Component::from_file(engine, format!("{}/composer_test_producer.wasm", dir)).unwrap();
    let consumer_component =
        Component::from_file(engine, format!("{}/composer_test_consumer.wasm", dir)).unwrap();

    // 1. Instantiate producer
    let mut linker_producer: Linker<TestState> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker_producer).unwrap();
    let mut store_producer = Store::new(engine, TestState::new());
    let producer_instance = linker_producer
        .instantiate_async(&mut store_producer, &producer_component)
        .await
        .unwrap();
    let mut composable_producer = ComposableInstance::new(producer_instance, store_producer);

    // 2. Create consumer linker, link producer's "add" export, then instantiate
    let mut linker_consumer: Linker<TestState> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker_consumer).unwrap();
    {
        let mut root = linker_consumer.root();
        composable_producer.link_export("add", &mut root).unwrap();
    }
    let mut store_consumer = Store::new(engine, TestState::new());
    let consumer_instance = linker_consumer
        .instantiate_async(&mut store_consumer, &consumer_component)
        .await
        .unwrap();
    let composable_consumer = ComposableInstance::new(consumer_instance, store_consumer);

    // 3. Build composition
    let producer_desc = ComposableDescriptor::new("producer", composable_producer);
    let consumer_desc = ComposableDescriptor::new("consumer", composable_consumer);

    let mut builder = CompositionBuilder::new();
    builder.add(producer_desc);
    builder.add(consumer_desc);
    builder.build().unwrap()
}

/// Cross-component call via async engine (no concurrency).
///
/// Flow: composition.call("run") -> consumer.run() -> add(20, 22) -> producer.add -> 42
#[tokio::test(flavor = "multi_thread")]
async fn test_cross_component_call_async() {
    let mut config = Config::new();
    config.wasm_component_model(true);
    let engine = Engine::new(&config).unwrap();

    let composition = make_composition(&engine).await;

    let run = composition.get_func(None, "run").unwrap();
    let mut results = vec![Val::S32(0)];
    run.call(&[], &mut results).await.unwrap();
    assert_eq!(results[0], Val::S32(42));
}

/// Cross-component call via async engine with concurrency support.
///
/// Flow: composition.call("run") -> consumer.run() -> add(20, 22) -> producer.add -> 42
#[cfg(feature = "component-model-async")]
#[tokio::test(flavor = "multi_thread")]
async fn test_cross_component_call_concurrent() {
    let mut config = Config::new();
    config.wasm_component_model(true);
    config.wasm_component_model_async(true);
    let engine = Engine::new(&config).unwrap();

    let composition = make_composition(&engine).await;

    let run = composition.get_func(None, "run").unwrap();
    let mut results = vec![Val::S32(0)];
    run.call(&[], &mut results).await.unwrap();
    assert_eq!(results[0], Val::S32(42));
}
