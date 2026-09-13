mod probe;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    probe::main()
}
