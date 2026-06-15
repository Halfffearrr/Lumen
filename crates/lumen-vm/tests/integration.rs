//! End-to-end integration tests: compile and run each bundled example script
//! through the whole pipeline and check its output. These guard against any
//! stage (lexer → parser → resolver → compiler → VM) regressing on real programs.

use std::path::PathBuf;

use lumen_vm::interpret;

/// Load an example script from the workspace `examples/` directory.
fn example(name: &str) -> String {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("../../examples");
    path.push(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {path:?}: {e}"))
}

/// Run a source string and return everything it printed.
fn run(src: &str) -> String {
    interpret(src)
        .expect("program should run without error")
        .output
}

#[test]
fn fizzbuzz_example() {
    let out = run(&example("fizzbuzz.lm"));
    assert_eq!(
        out,
        "1\n2\nFizz\n4\nBuzz\nFizz\n7\n8\nFizz\nBuzz\n11\nFizz\n13\n14\nFizzBuzz\n"
    );
}

#[test]
fn tour_example() {
    let out = run(&example("tour.lm"));
    assert!(out.contains("total = 66"));
    assert!(out.contains("len = 4"));
    assert!(out.contains("counted to 5"));
}

#[test]
fn fib_example() {
    let out = run(&example("fib.lm"));
    assert!(out.contains("fib(0) = 0"));
    assert!(out.contains("fib(10) = 55"));
}

#[test]
fn closures_example() {
    let out = run(&example("closures.lm"));
    assert!(out.contains("a: 1 2 3"));
    assert!(out.contains("b: 1")); // independent counter
    assert!(out.contains("add10(5) = 15"));
    assert!(out.contains("squares: 1 4 9"));
}

#[test]
fn calc_example_self_hosting() {
    let out = run(&example("calc.lm"));
    assert!(out.contains("2 + 3 * 4         = 14"));
    assert!(out.contains("(2 + 3) * 4       = 20"));
    assert!(out.contains("100 / (2 + 3) / 2 = 10"));
}

#[test]
fn bench_example_runs() {
    let out = run(&example("bench.lm"));
    assert!(out.contains("fib(30) = 832040"));
}
