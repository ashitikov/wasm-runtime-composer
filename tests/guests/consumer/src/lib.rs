mod bindings {
    wit_bindgen::generate!({
        path: "../wit",
        world: "consumer",
    });

    use super::Consumer;
    export!(Consumer);
}

struct Consumer;

impl bindings::Guest for Consumer {
    fn run_add() -> i32 {
        bindings::add(20, 22)
    }

    async fn run_ping() -> i32 {
        bindings::composer::test::iproducer::ping(42).await
    }
}

impl bindings::exports::composer::test::iconsumer::Guest for Consumer {
    fn run_add() -> i32 {
        bindings::add(20, 22)
    }

    async fn run_ping() -> i32 {
        bindings::composer::test::iproducer::ping(42).await
    }
}
