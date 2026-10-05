fn main() -> std::process::ExitCode {
    // Rust ignores SIGPIPE, so a write to a closed pipe returns EPIPE and
    // `println!` panics. Restore the default: the kernel ends the process
    // with signal 13, which is what `bpm | head` should do.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    bpm3::cli::run()
}
