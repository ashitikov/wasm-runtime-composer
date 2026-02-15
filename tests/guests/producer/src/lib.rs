mod bindings {
    wit_bindgen::generate!({
        path: "../wit",
        world: "producer",
    });

    use super::Producer;
    export!(Producer);
}

struct Producer;

impl bindings::Guest for Producer {
    fn add(a: i32, b: i32) -> i32 {
        a + b
    }
}
