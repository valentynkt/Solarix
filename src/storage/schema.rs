/// Sanitize a name for use as a PostgreSQL identifier.
///
/// - Strips non-alphanumeric chars except underscores
/// - Lowercases the result
/// - Prepends `_` if starts with digit
/// - Falls back to `_unnamed` if empty after sanitization
/// - Truncates to 63 bytes on byte boundaries
pub fn sanitize_identifier(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_')
        .collect::<String>()
        .to_lowercase();

    let sanitized = if sanitized.starts_with(|c: char| c.is_ascii_digit()) {
        format!("_{sanitized}")
    } else {
        sanitized
    };

    let sanitized = if sanitized.is_empty() {
        "_unnamed".to_string()
    } else {
        sanitized
    };

    truncate_to_bytes(&sanitized, 63)
}

/// Derive a schema name from IDL name and program ID.
///
/// Format: `{sanitized_name}_{lowercase_first_8_of_base58_program_id}`
pub fn derive_schema_name(idl_name: &str, program_id: &str) -> String {
    let name_part = sanitize_identifier(idl_name);
    let id_prefix: String = program_id
        .chars()
        .take(8)
        .collect::<String>()
        .to_lowercase();
    let full = format!("{name_part}_{id_prefix}");
    truncate_to_bytes(&full, 63)
}

fn truncate_to_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_normal_input() {
        assert_eq!(sanitize_identifier("MyProgram"), "myprogram");
    }

    #[test]
    fn sanitize_with_underscores() {
        assert_eq!(sanitize_identifier("my_cool_program"), "my_cool_program");
    }

    #[test]
    fn sanitize_strips_special_chars() {
        assert_eq!(sanitize_identifier("hello-world!@#$%"), "helloworld");
    }

    #[test]
    fn sanitize_digit_first() {
        assert_eq!(sanitize_identifier("123program"), "_123program");
    }

    #[test]
    fn sanitize_empty_input() {
        assert_eq!(sanitize_identifier(""), "_unnamed");
    }

    #[test]
    fn sanitize_all_special_chars() {
        assert_eq!(sanitize_identifier("!@#$%^&*"), "_unnamed");
    }

    #[test]
    fn sanitize_unicode() {
        // é is alphanumeric in Unicode, so it passes the filter
        assert_eq!(sanitize_identifier("café"), "café");
    }

    #[test]
    fn sanitize_truncate_63_bytes() {
        let long = "a".repeat(100);
        let result = sanitize_identifier(&long);
        assert_eq!(result.len(), 63);
        assert!(result.is_char_boundary(result.len()));
    }

    #[test]
    fn derive_schema_name_basic() {
        let name = derive_schema_name(
            "token_program",
            "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
        );
        assert_eq!(name, "token_program_tokenkeg");
    }

    #[test]
    fn derive_schema_name_short_program_id() {
        let name = derive_schema_name("test", "ABC");
        assert_eq!(name, "test_abc");
    }

    #[test]
    fn derive_schema_name_truncates_to_63() {
        let long_name = "a".repeat(60);
        let name = derive_schema_name(&long_name, "12345678ABCDEF");
        assert!(name.len() <= 63);
        assert!(name.is_char_boundary(name.len()));
    }
}
