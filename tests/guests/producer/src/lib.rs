mod bindings {
    wit_bindgen::generate!({
        path: "../wit",
        world: "producer",
    });

    use super::Producer;
    export!(Producer);
}

use crate::bindings::exports::composer::test::iproducer;

struct Producer;

struct Pong {
    value: i32,
}

impl bindings::Guest for Producer {
    fn add(a: i32, b: i32) -> i32 {
        a + b
    }
}

impl bindings::exports::composer::test::iproducer::Guest for Producer {
    type Pong = Pong;

    async fn ping(ping: i32) -> i32 {
        ping
    }

    fn get_pong(ping: i32) -> bindings::exports::composer::test::iproducer::Pong {
        bindings::exports::composer::test::iproducer::Pong::new(Pong { value: ping })
    }

    fn get_pong_nested(ping: i32) -> iproducer::NestedPong {
        iproducer::NestedPong {
            pong: bindings::exports::composer::test::iproducer::Pong::new(Pong { value: ping }),
        }
    }

    async fn get_pong_res(pong: iproducer::Pong) -> i32 {
        pong.get::<Pong>().value
    }

    async fn get_pong_res_nested(pong: iproducer::NestedPong) -> i32 {
        pong.pong.get::<Pong>().value
    }

    async fn get_pong_res_borrow(pong: iproducer::PongBorrow<'_>) -> i32 {
        pong.get::<Pong>().value
    }
}

impl bindings::exports::composer::test::iproducer::GuestPong for Pong {
    async fn get(&self) -> i32 {
        self.value
    }
}
