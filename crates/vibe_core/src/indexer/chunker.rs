use std::path::Path;
use tree_sitter::{Language, Node, Parser};

#[derive(Debug, Clone)]
pub struct CodeChunk {
    pub file_path: String,
    pub symbol_name: String,
    pub chunk_type: String,
    pub content: String,
    pub start_line: usize,
    pub end_line: usize,
}

pub fn language_for_path(path: &Path) -> Option<Language> {
    let ext = path.extension()?.to_str()?;
    match ext {
        "rs" => Some(tree_sitter_rust::LANGUAGE.into()),
        "py" => Some(tree_sitter_python::LANGUAGE.into()),
        "js" | "jsx" | "mjs" | "cjs" => Some(tree_sitter_javascript::LANGUAGE.into()),
        "ts" => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        "tsx" => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
        "go" => Some(tree_sitter_go::LANGUAGE.into()),
        "java" => Some(tree_sitter_java::LANGUAGE.into()),
        _ => None,
    }
}

const TARGET_KINDS: &[&str] = &[
    "function_item",
    "impl_item",
    "struct_item",
    "enum_item",
    "mod_item",
    "function_definition",
    "class_definition",
    "decorated_definition",
    "function_declaration",
    "class_declaration",
    "method_definition",
    "export_statement",
    "method_declaration",
    "constructor_declaration",
    "interface_declaration",
    "type_alias_declaration",
    "function_declaration",
    "method_declaration",
    "type_declaration",
];

pub fn chunk_source_file(file_path: &str, source_code: &str, lang: Language) -> Vec<CodeChunk> {
    let mut parser = Parser::new();
    if parser.set_language(&lang).is_err() {
        return fallback_whole_file(file_path, source_code);
    }
    let Some(tree) = parser.parse(source_code, None) else {
        return fallback_whole_file(file_path, source_code);
    };

    let mut chunks = Vec::new();
    walk(tree.root_node(), source_code, file_path, &mut chunks);

    if chunks.is_empty() {
        return fallback_whole_file(file_path, source_code);
    }
    chunks
}

fn walk(node: Node, source: &str, file_path: &str, out: &mut Vec<CodeChunk>) {
    let kind = node.kind();
    if TARGET_KINDS.contains(&kind) {
        if let Some(content) = source.get(node.start_byte()..node.end_byte()) {
            // Skip tiny export wrappers without body interest
            if content.len() > 20 {
                out.push(CodeChunk {
                    file_path: file_path.to_string(),
                    symbol_name: extract_identifier(&node, source),
                    chunk_type: kind.to_string(),
                    content: content.to_string(),
                    start_line: node.start_position().row + 1,
                    end_line: node.end_position().row + 1,
                });
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, source, file_path, out);
    }
}

fn extract_identifier(node: &Node, source: &str) -> String {
    for field in ["name", "declarator"] {
        if let Some(id_node) = node.child_by_field_name(field) {
            if let Some(name) = source.get(id_node.start_byte()..id_node.end_byte()) {
                let cleaned = name
                    .split_whitespace()
                    .next()
                    .unwrap_or(name)
                    .trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
                if !cleaned.is_empty() {
                    return cleaned.to_string();
                }
            }
        }
    }
    // Fall back: first identifier child
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind().contains("identifier") || child.kind() == "property_identifier" {
            if let Some(name) = source.get(child.start_byte()..child.end_byte()) {
                return name.to_string();
            }
        }
    }
    "anonymous".to_string()
}

fn fallback_whole_file(file_path: &str, source_code: &str) -> Vec<CodeChunk> {
    if source_code.trim().is_empty() {
        return Vec::new();
    }
    let lines = source_code.lines().count().max(1);
    vec![CodeChunk {
        file_path: file_path.to_string(),
        symbol_name: Path::new(file_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("file")
            .to_string(),
        chunk_type: "file".to_string(),
        content: source_code.to_string(),
        start_line: 1,
        end_line: lines,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_rust_functions() {
        let src = r#"
pub fn alpha() -> i32 { 1 }
pub fn beta(x: i32) -> i32 { x + 1 }
"#;
        let chunks = chunk_source_file("src/lib.rs", src, tree_sitter_rust::LANGUAGE.into());
        assert!(chunks.len() >= 2);
        assert!(chunks.iter().any(|c| c.symbol_name == "alpha"));
    }
}
