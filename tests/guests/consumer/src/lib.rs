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
    fn run() -> i32 {
        bindings::add(20, 22)
    }
}
