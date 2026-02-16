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

    async fn run_pong() -> i32 {
        let pong = bindings::composer::test::iproducer::get_pong(42);
        pong.get().await
    }

    async fn run_pong_res() -> i32 {
        let pong = bindings::composer::test::iproducer::get_pong(42);
        bindings::composer::test::iproducer::get_pong_res(pong).await
    }
}

impl bindings::exports::composer::test::iconsumer::Guest for Consumer {
    fn run_add() -> i32 {
        bindings::add(20, 22)
    }

    async fn run_ping() -> i32 {
        bindings::composer::test::iproducer::ping(42).await
    }

    async fn run_pong() -> i32 {
        let pong = bindings::composer::test::iproducer::get_pong(42);
        pong.get().await
    }

    async fn run_pong_res() -> i32 {
        let pong = bindings::composer::test::iproducer::get_pong(42);
        bindings::composer::test::iproducer::get_pong_res(pong).await
    }
}
