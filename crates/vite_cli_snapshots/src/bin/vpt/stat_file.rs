pub fn run(args: &[String]) {
    // Reports the entry type, not just existence, so migrated `test -f` /
    // `test -d` assertions keep their predicate fidelity in snapshots.
    for file in args {
        match std::fs::metadata(file) {
            Ok(meta) if meta.is_dir() => println!("{file}: dir"),
            Ok(_) => println!("{file}: file"),
            Err(_) => println!("{file}: missing"),
        }
    }
}
