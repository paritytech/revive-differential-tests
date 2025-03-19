use revive_differential_testing_format::metadata::Metadata;

fn main() {
    let example_def = include_str!("../../../test.json");

    let metadata: Metadata = serde_json::from_str(example_def).unwrap();

    println!("{metadata:?}");
}
