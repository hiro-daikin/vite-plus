pub fn run(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let (recursive, paths) = match args {
        [flag, src, dst] if flag == "-r" => (true, [src.as_str(), dst.as_str()]),
        [src, dst] => (false, [src.as_str(), dst.as_str()]),
        _ => return Err("Usage: vpt cp [-r] <src> <dst>".into()),
    };

    let src = std::path::Path::new(paths[0]);
    let dst = std::path::Path::new(paths[1]);
    if src.is_dir() {
        if !recursive {
            return Err("copying a directory requires -r".into());
        }
        copy_dir_recursive(src, dst)?;
    } else {
        // `cp file existing-dir` copies INTO the directory, like real cp.
        let target = if dst.is_dir() {
            dst.join(src.file_name().ok_or("source has no file name")?)
        } else {
            dst.to_path_buf()
        };
        std::fs::copy(src, target)?;
    }
    Ok(())
}

fn copy_dir_recursive(
    src: &std::path::Path,
    dst: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else if file_type.is_file() {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}
