use rand::Rng;

pub fn new_id(prefix: &str) -> String {
    let mut rng = rand::thread_rng();
    let n: u32 = rng.gen();
    format!("{prefix}_{n:08x}")
}
