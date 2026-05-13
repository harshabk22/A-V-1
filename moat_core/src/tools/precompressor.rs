/// Pre-compress terminal output BEFORE adding to context.
/// Raw output > 500 chars is never stored in context.
pub struct OutputPrecompressor;

impl OutputPrecompressor {
    pub fn compress_terminal_output(
        command: &str,
        raw_output: &str,
        exit_code: i32,
    ) -> String {
        let line_count = raw_output.lines().count();
        let byte_count = raw_output.len();

        if byte_count <= 500 {
            return format!(
                "[process:spawn] `{}` → exit {}, output:\n{}",
                command, exit_code, raw_output
            );
        }

        let first_3_lines: Vec<&str> = raw_output.lines().take(3).collect();
        let last_2_lines: Vec<&str> = raw_output.lines()
            .rev()
            .take(2)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();

        format!(
            "[process:spawn] `{}` → exit {}, {} lines, {} bytes\n             First lines: {}\n             ...\n             Last lines: {}",
            command,
            exit_code,
            line_count,
            byte_count,
            first_3_lines.join(" | "),
            last_2_lines.join(" | ")
        )
    }

    pub fn compress_file_read(path: &str, content: &str) -> String {
        let line_count = content.lines().count();
        let byte_count = content.len();

        if byte_count <= 1000 {
            return format!("[fs:read] {} →\n{}", path, content);
        }

        let first_10: Vec<&str> = content.lines().take(10).collect();
        let last_5: Vec<&str> = content.lines()
            .rev()
            .take(5)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();

        format!(
            "[fs:read] {} → {} lines, {} bytes\n             First 10 lines:\n{}\n             ...\n             Last 5 lines:\n{}",
            path,
            line_count,
            byte_count,
            first_10.join("\n"),
            last_5.join("\n")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_small_output_kept_intact() {
        let result = OutputPrecompressor::compress_terminal_output(
            "echo hello",
            "hello",
            0,
        );
        assert!(result.contains("hello"));
        assert!(result.contains("exit 0"));
    }

    #[test]
    fn test_large_output_compressed() {
        let large_output = (0..100)
            .map(|i| format!("line {}", i))
            .collect::<Vec<_>>()
            .join("\n");

        let result = OutputPrecompressor::compress_terminal_output(
            "cat bigfile.txt",
            &large_output,
            0,
        );
        assert!(result.contains("100 lines"));
        assert!(result.contains("First lines"));
        assert!(result.contains("Last lines"));
        assert!(!result.contains("line 50"));
    }
}