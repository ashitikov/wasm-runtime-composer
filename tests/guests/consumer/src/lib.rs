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
    fn passthrough_pong() -> bindings::composer::test::iproducer::Pong {
        bindings::composer::test::iproducer::get_pong(42)
    }

    async fn forge_pong(raw: u32) -> i32 {
        // Fabricate a handle from a raw guessed number and pass it on. The
        // ABI validates the handle against this instance's own table when
        // it is lowered into the call — an unowned number traps here.
        let forged = unsafe { bindings::composer::test::iproducer::Pong::from_handle(raw) };
        bindings::composer::test::iproducer::get_pong_res(forged).await
    }

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

    async fn run_pong_res_borrow() -> i32 {
        let pong = bindings::composer::test::iproducer::get_pong(42);
        bindings::composer::test::iproducer::get_pong_res_borrow(&pong).await
    }

    async fn run_pong_res_nested() -> i32 {
        let nested = bindings::composer::test::iproducer::get_pong_nested(42);
        bindings::composer::test::iproducer::get_pong_res_nested(nested).await
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

    async fn run_pong_res_borrow() -> i32 {
        let pong = bindings::composer::test::iproducer::get_pong(42);
        bindings::composer::test::iproducer::get_pong_res_borrow(&pong).await
    }

    async fn run_pong_res_nested() -> i32 {
        let nested = bindings::composer::test::iproducer::get_pong_nested(42);
        bindings::composer::test::iproducer::get_pong_res_nested(nested).await
    }
}
