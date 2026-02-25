use wasmtime::component::{Component, Linker, ResourceTable, Val};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use wasm_runtime_composer::{
    ComposableComponent, ComposableDescriptor, Composer, ResourceProxyView,
};

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

fn make_producer_linker(engine: &Engine) -> Linker<TestState> {
    let mut linker = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker).unwrap();
    linker
}

fn make_consumer_linker(engine: &Engine) -> Linker<TestState> {
    let mut linker = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker).unwrap();
    linker
}

/// Helper: create producer and consumer as ComposableComponents, let Composer wire them up.
async fn make_composition(engine: &Engine) -> wasm_runtime_composer::Composition {
    let dir = guests_dir();

    let producer_component =
        Component::from_file(engine, format!("{}/composer_test_producer.wasm", dir)).unwrap();
    let consumer_component =
        Component::from_file(engine, format!("{}/composer_test_consumer.wasm", dir)).unwrap();

    let engine_clone = engine.clone();
    let producer = ComposableComponent::new(
        producer_component,
        make_producer_linker(engine),
        move || Store::new(&engine_clone, TestState::new()),
    );

    let engine_clone = engine.clone();
    let consumer = ComposableComponent::new(
        consumer_component,
        make_consumer_linker(engine),
        move || Store::new(&engine_clone, TestState::new()),
    );

    let producer_desc = ComposableDescriptor::new("producer", producer);
    let consumer_desc = ComposableDescriptor::new("consumer", consumer);

    let mut composer = Composer::new();
    composer.add(producer_desc);
    composer.add(consumer_desc);
    composer.compose().await.unwrap()
}

/// Sync cross-component call via ComposableComponent: consumer.run_add() -> producer.add(20, 22) -> 42
#[tokio::test(flavor = "multi_thread")]
async fn test_component_cross_call_sync() {
    let engine = test_engine();
    let composition = make_composition(&engine).await;

    let run_add = composition.get_func(None, "run-add").unwrap();
    let mut results = vec![Val::S32(0)];
    run_add.call(&[], &mut results).await.unwrap();
    assert_eq!(results[0], Val::S32(42));
}

/// Async cross-component call via ComposableComponent: consumer.run_ping() -> producer.ping(42) -> 42
#[tokio::test(flavor = "multi_thread")]
async fn test_component_cross_call_async() {
    let engine = test_engine();
    let composition = make_composition(&engine).await;

    let run_ping = composition.get_func(None, "run-ping").unwrap();
    let mut results = vec![Val::S32(0)];
    run_ping.call(&[], &mut results).await.unwrap();
    assert_eq!(results[0], Val::S32(42));
}

/// Interface export via ComposableComponent: composer:test/iconsumer.run-add
#[tokio::test(flavor = "multi_thread")]
async fn test_component_interface_export() {
    let engine = test_engine();
    let composition = make_composition(&engine).await;

    let run_add = composition
        .get_func(Some("composer:test/iconsumer"), "run-add")
        .unwrap();
    let mut results = vec![Val::S32(0)];
    run_add.call(&[], &mut results).await.unwrap();
    assert_eq!(results[0], Val::S32(42));
}

/// Resource round-trip via ComposableComponent
#[tokio::test(flavor = "multi_thread")]
async fn test_component_resource() {
    let engine = test_engine();
    let composition = make_composition(&engine).await;

    let run_pong = composition.get_func(None, "run-pong").unwrap();
    let mut results = vec![Val::S32(0)];
    run_pong.call(&[], &mut results).await.unwrap();
    assert_eq!(results[0], Val::S32(42));
}

/// Single-threaded runtime via ComposableComponent
#[tokio::test(flavor = "current_thread")]
async fn test_component_single_thread() {
    let engine = test_engine();
    let composition = make_composition(&engine).await;

    let run_add = composition.get_func(None, "run-add").unwrap();
    let mut results = vec![Val::S32(0)];
    run_add.call(&[], &mut results).await.unwrap();
    assert_eq!(results[0], Val::S32(42));
}
