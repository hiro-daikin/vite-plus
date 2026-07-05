/// chmod `<octal-mode>|+x` `<path>`
///
/// Sets POSIX permission bits; `+x` adds the execute bits to the current
/// mode (the common legacy-fixture form, `chmod +x hook.mjs`). On Windows
/// this is a no-op so fixtures that prepare executable files stay
/// platform-identical.
pub fn run(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args.len() != 2 {
        return Err("Usage: vpt chmod <octal-mode>|+x <path>".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = if args[0] == "+x" {
            std::fs::metadata(&args[1])?.permissions().mode() | 0o111
        } else {
            u32::from_str_radix(&args[0], 8)?
        };
        std::fs::set_permissions(&args[1], std::fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}
