use wasmtime::component::{Component, Linker, ResourceTable, Val};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use wasm_runtime_composer::{Composable, ComposableDescriptor, ComposableInstance, Composer, ResourceProxyView, ComposableLinker};

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

impl ResourceProxyView for TestState {
    fn proxy_table(&mut self) -> &mut ResourceTable {
        &mut self.table
    }
}

fn guests_dir() -> String {
    format!(
        "{}/tests/guests/target/wasm32-wasip2/release",
        env!("CARGO_MANIFEST_DIR")
    )
}

fn test_engine() -> Engine {
    let mut config = Config::new();
    config.wasm_component_model(true);
    config.wasm_component_model_async(true);
    Engine::new(&config).unwrap()
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
    let mut composable_producer = ComposableInstance::from_existing(producer_instance, store_producer);

    // 2. Create consumer linker, link producer exports, then instantiate
    let mut linker_consumer: Linker<TestState> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker_consumer).unwrap();
    {
        let root = linker_consumer.root();
        let mut composable_linker = ComposableLinker::new(root);
        composable_producer.link_export("add", &mut composable_linker).unwrap();
        composable_producer
            .link_export("composer:test/iproducer", &mut composable_linker)
            .unwrap();
    }
    let mut store_consumer = Store::new(engine, TestState::new());
    let consumer_instance = linker_consumer
        .instantiate_async(&mut store_consumer, &consumer_component)
        .await
        .unwrap();
    let composable_consumer = ComposableInstance::from_existing(consumer_instance, store_consumer);

    // 3. Build composition
    let producer_desc = ComposableDescriptor::new("producer", composable_producer);
    let consumer_desc = ComposableDescriptor::new("consumer", composable_consumer);

    let mut composer = Composer::new();
    composer.add(producer_desc);
    composer.add(consumer_desc);
    composer.compose().await.unwrap()
}

/// Sync cross-component call: consumer.run_add() -> producer.add(20, 22) -> 42
#[tokio::test(flavor = "multi_thread")]
async fn test_cross_component_call_sync() {
    let engine = test_engine();
    let composition = make_composition(&engine).await;

    let run_add = composition.get_func(None, "run-add").unwrap();
    let mut results = vec![Val::S32(0)];
    run_add.call(&[], &mut results).await.unwrap();
    assert_eq!(results[0], Val::S32(42));
}

/// Async cross-component call: consumer.run_ping() -> producer.ping(42) -> 42
/// Exercises the concurrent call path (async func in WIT).
#[tokio::test(flavor = "multi_thread")]
async fn test_cross_component_call_async() {
    let engine = test_engine();
    let composition = make_composition(&engine).await;

    let run_ping = composition.get_func(None, "run-ping").unwrap();
    let mut results = vec![Val::S32(0)];
    run_ping.call(&[], &mut results).await.unwrap();
    assert_eq!(results[0], Val::S32(42));
}

/// Call a function from an exported interface: composer:test/iconsumer.run-add
#[tokio::test(flavor = "multi_thread")]
async fn test_interface_export_sync() {
    let engine = test_engine();
    let composition = make_composition(&engine).await;

    let run_add = composition
        .get_func(Some("composer:test/iconsumer"), "run-add")
        .unwrap();
    let mut results = vec![Val::S32(0)];
    run_add.call(&[], &mut results).await.unwrap();
    assert_eq!(results[0], Val::S32(42));
}

/// Call an async function from an exported interface: composer:test/iconsumer.run-ping
#[tokio::test(flavor = "multi_thread")]
async fn test_interface_export_async() {
    let engine = test_engine();
    let composition = make_composition(&engine).await;

    let run_ping = composition
        .get_func(Some("composer:test/iconsumer"), "run-ping")
        .unwrap();
    let mut results = vec![Val::S32(0)];
    run_ping.call(&[], &mut results).await.unwrap();
    assert_eq!(results[0], Val::S32(42));
}

/// Both sync and async cross-component calls on a single-threaded tokio runtime.
#[tokio::test(flavor = "current_thread")]
async fn test_single_thread_sync() {
    let engine = test_engine();
    let composition = make_composition(&engine).await;

    let run_add = composition.get_func(None, "run-add").unwrap();
    let mut results = vec![Val::S32(0)];
    run_add.call(&[], &mut results).await.unwrap();
    assert_eq!(results[0], Val::S32(42));
}

/// Resource round-trip via method: consumer.run_pong() -> producer.get_pong(42) -> pong.get() -> 42
#[tokio::test(flavor = "multi_thread")]
async fn test_cross_component_resource() {
    let engine = test_engine();
    let composition = make_composition(&engine).await;

    let run_pong = composition.get_func(None, "run-pong").unwrap();
    let mut results = vec![Val::S32(0)];
    run_pong.call(&[], &mut results).await.unwrap();
    assert_eq!(results[0], Val::S32(42));
}

/// Resource round-trip via function: consumer.run_pong_res() -> producer.get_pong(42) -> producer.get_pong_res(pong) -> 42
#[tokio::test(flavor = "multi_thread")]
async fn test_cross_component_resource_func() {
    let engine = test_engine();
    let composition = make_composition(&engine).await;

    let run_pong_res = composition.get_func(None, "run-pong-res").unwrap();
    let mut results = vec![Val::S32(0)];
    run_pong_res.call(&[], &mut results).await.unwrap();
    assert_eq!(results[0], Val::S32(42));
}

/// Resource borrow round-trip: consumer.run_pong_res_borrow() -> producer.get_pong(42) -> producer.get_pong_res_borrow(&pong) -> 42
#[tokio::test(flavor = "multi_thread")]
async fn test_cross_component_resource_borrow() {
    let engine = test_engine();
    let composition = make_composition(&engine).await;

    let run_pong_res_borrow = composition.get_func(None, "run-pong-res-borrow").unwrap();
    let mut results = vec![Val::S32(0)];
    run_pong_res_borrow.call(&[], &mut results).await.unwrap();
    assert_eq!(results[0], Val::S32(42));
}

/// Nested resource round-trip: consumer.run_pong_res_nested() -> producer.get_pong_nested(42) -> producer.get_pong_res_nested(nested-pong) -> 42
#[tokio::test(flavor = "multi_thread")]
async fn test_cross_component_resource_nested() {
    let engine = test_engine();
    let composition = make_composition(&engine).await;

    let func = composition.get_func(None, "run-pong-res-nested").unwrap();
    let mut results = vec![Val::S32(0)];
    func.call(&[], &mut results).await.unwrap();
    assert_eq!(results[0], Val::S32(42));
}

#[tokio::test(flavor = "current_thread")]
async fn test_single_thread_async() {
    let engine = test_engine();
    let composition = make_composition(&engine).await;

    let run_ping = composition.get_func(None, "run-ping").unwrap();
    let mut results = vec![Val::S32(0)];
    run_ping.call(&[], &mut results).await.unwrap();
    assert_eq!(results[0], Val::S32(42));
}
