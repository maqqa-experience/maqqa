/// Missing HashMap import — heal should add `use std::collections::HashMap;`
pub fn counts(items: &[&str]) -> HashMap<String, usize> {
    let mut m = HashMap::new();
    for item in items {
        *m.entry((*item).to_string()).or_insert(0) += 1;
    }
    m
}
