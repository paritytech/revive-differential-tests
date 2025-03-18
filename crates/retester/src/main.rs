use revive_differential_testing_format::case::Case;

fn main() {
    let example_def = include_str!("../../../test.json");

    let case: Case = serde_json::from_str(example_def).unwrap();

    println!("{case:?}");
}
