//! `ferramp-plughost`: loads classic Windows player plugins on behalf of
//! Ferramp and talks to it over stdin/stdout (see `ferramp-plugproto`). It
//! runs natively on Windows and under Wine elsewhere.

#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(windows)]
mod win;

fn main() {
    #[cfg(windows)]
    std::process::exit(win::run());
    #[cfg(not(windows))]
    {
        eprintln!(
            "ferramp-plughost is a Windows program; Ferramp runs it under Wine on this system."
        );
        std::process::exit(2);
    }
}
