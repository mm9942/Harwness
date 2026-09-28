//! A program that never installs `builtins::BuiltinDefaults` gets a clear
//! error from every compiler entry point instead of a compile over an empty
//! catalog. Its own test binary: nothing in it installs the defaults.

use std::path::PathBuf;

use harw_agent_compiler::discovery::SourceSet;
use harw_agent_compiler::rights::BuiltinCeilings;
use harw_agent_compiler::{CompileError, Compiler, CompilerEnv, CompilerOptions};

fn assert_names_the_fix(error: &CompileError) {
    let text = error.to_string();
    assert!(
        text.contains("harw_registry_defaults::compiler_defaults::install()"),
        "{text}"
    );
}

#[test]
fn test_compiler_entry_points_require_installed_defaults() -> Result<(), String> {
    let error = SourceSet::discover(&[])
        .err()
        .ok_or("discovery without defaults must fail")?;
    assert_names_the_fix(&error);

    let error = BuiltinCeilings::load(time::OffsetDateTime::UNIX_EPOCH)
        .err()
        .ok_or("ceilings without defaults must fail")?;
    assert_names_the_fix(&error);

    let env = CompilerEnv::isolated(PathBuf::from("/nonexistent/home"), PathBuf::from("/"));
    let error = Compiler::new(env, CompilerOptions::default())
        .err()
        .ok_or("a compiler without defaults must fail")?;
    assert_names_the_fix(&error);
    Ok(())
}
