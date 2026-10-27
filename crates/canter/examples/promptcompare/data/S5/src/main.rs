// Print a greeting for the person named on the command line.
fn main() {
    let name = std::env::args().nth(1).unwrap_or_else(|| "world".into());
    println!("Hello, {name}!");
}
