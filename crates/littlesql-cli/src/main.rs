fn main() {
    let args: Vec<String> = std::env::args().collect();
    let db = littlesql::Database::open_readonly(&args[1]).unwrap();
    match db.query(&args[2]) {
        Ok(rs) => {
            for r in rs.rows {
                println!(
                    "{}",
                    r.iter().map(|v| v.to_text()).collect::<Vec<_>>().join("|")
                );
            }
        }
        Err(e) => eprintln!("Error: {e}"),
    }
}
