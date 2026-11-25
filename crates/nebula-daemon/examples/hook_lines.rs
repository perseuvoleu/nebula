//! Prints one per-prompt hook line at a fixed input, for
//! `.agents/context-budget.json`.

use nebula_daemon::hooks::{siblings_instruction, twin_instruction, AUTO_TITLE_INSTRUCTION};

fn main() {
    let line = match std::env::args().nth(1).as_deref() {
        Some("siblings") => siblings_instruction(9),
        Some("twin") => twin_instruction(
            "apples-macbook-pro",
            "nebula",
            &[
                ("main".to_string(), 3),
                ("feature/dx-gates".to_string(), 1),
                ("fix/worktree-sync".to_string(), 0),
            ],
        ),
        Some("twin-empty") => twin_instruction("apples-macbook-pro", "nebula", &[]),
        Some("auto-title") => AUTO_TITLE_INSTRUCTION.to_string(),
        _ => {
            eprintln!("usage: hook_lines siblings | twin | twin-empty | auto-title");
            std::process::exit(2);
        }
    };
    print!("{line}");
}
