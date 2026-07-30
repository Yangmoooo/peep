use std::sync::OnceLock;
use std::time::Duration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DefaultColors {
    pub(crate) foreground: (u8, u8, u8),
    pub(crate) background: (u8, u8, u8),
}

static DEFAULT_COLORS: OnceLock<Option<DefaultColors>> = OnceLock::new();

pub(crate) fn set_default_colors(colors: Option<DefaultColors>) {
    let _ = DEFAULT_COLORS.set(colors);
}

pub(crate) fn default_colors() -> Option<DefaultColors> { DEFAULT_COLORS.get().copied().flatten() }

pub(crate) fn detect_default_colors(timeout: Duration) -> Option<DefaultColors> {
    imp::detect_default_colors(timeout)
}

#[cfg(unix)]
mod imp {
    use std::fs::OpenOptions;
    use std::io::{Read, Write};
    use std::os::unix::fs::OpenOptionsExt;
    use std::thread;
    use std::time::{Duration, Instant};

    use super::DefaultColors;

    pub(super) fn detect_default_colors(timeout: Duration) -> Option<DefaultColors> {
        let mut input =
            OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open("/dev/tty").ok()?;
        let mut output = OpenOptions::new().write(true).open("/dev/tty").ok()?;
        output.write_all(b"\x1B]10;?\x1B\\\x1B]11;?\x1B\\").ok()?;
        output.flush().ok()?;

        let deadline = Instant::now() + timeout;
        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 256];
        loop {
            loop {
                match input.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(count) => buffer.extend_from_slice(&chunk[..count]),
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(_) => return None,
                }
            }
            if let Some(colors) = parse_default_colors(&buffer) {
                return Some(colors);
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(2));
        }
    }

    fn parse_default_colors(buffer: &[u8]) -> Option<DefaultColors> {
        Some(DefaultColors {
            foreground: parse_osc_color(buffer, 10)?,
            background: parse_osc_color(buffer, 11)?,
        })
    }

    fn parse_osc_color(buffer: &[u8], slot: u8) -> Option<(u8, u8, u8)> {
        let prefix = format!("\x1B]{slot};");
        let start = buffer.windows(prefix.len()).position(|window| window == prefix.as_bytes())?;
        let rest = &buffer[start + prefix.len()..];
        let end = rest
            .iter()
            .position(|byte| *byte == 0x07)
            .or_else(|| rest.windows(2).position(|window| window == b"\x1B\\"))?;
        parse_osc_rgb(std::str::from_utf8(&rest[..end]).ok()?)
    }

    fn parse_osc_rgb(value: &str) -> Option<(u8, u8, u8)> {
        let (kind, channels) = value.trim().split_once(':')?;
        if !kind.eq_ignore_ascii_case("rgb") {
            return None;
        }
        let mut channels = channels.split('/');
        let red = parse_component(channels.next()?)?;
        let green = parse_component(channels.next()?)?;
        let blue = parse_component(channels.next()?)?;
        channels.next().is_none().then_some((red, green, blue))
    }

    fn parse_component(value: &str) -> Option<u8> {
        match value.len() {
            2 => u8::from_str_radix(value, 16).ok(),
            4 => u16::from_str_radix(value, 16).ok().map(|value| (value / 257) as u8),
            _ => None,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_terminal_colors_in_either_order() {
            assert_eq!(
                parse_default_colors(
                    b"\x1B]11;rgb:ffff/ffff/ffff\x07\x1B]10;rgb:1111/2222/3333\x1B\\"
                ),
                Some(DefaultColors { foreground: (17, 34, 51), background: (255, 255, 255) })
            );
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::time::Duration;

    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Console::{
        CONSOLE_SCREEN_BUFFER_INFOEX, GetConsoleScreenBufferInfoEx, GetStdHandle, STD_OUTPUT_HANDLE,
    };

    use super::DefaultColors;

    pub(super) fn detect_default_colors(_timeout: Duration) -> Option<DefaultColors> {
        // SAFETY: querying the process standard output handle does not transfer
        // ownership.
        let output = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
        if output.is_null() || output == INVALID_HANDLE_VALUE {
            return None;
        }
        // SAFETY: the structure is initialized with its required size before the API
        // call.
        let mut info = unsafe { std::mem::zeroed::<CONSOLE_SCREEN_BUFFER_INFOEX>() };
        info.cbSize = std::mem::size_of::<CONSOLE_SCREEN_BUFFER_INFOEX>() as u32;
        // SAFETY: `output` is a borrowed console handle and `info` is a valid output
        // pointer.
        if unsafe { GetConsoleScreenBufferInfoEx(output, &mut info) } == 0 {
            return None;
        }
        let foreground = (info.wAttributes & 0x0f) as usize;
        let background = ((info.wAttributes >> 4) & 0x0f) as usize;
        Some(DefaultColors {
            foreground: decode_color_ref(info.ColorTable[foreground]),
            background: decode_color_ref(info.ColorTable[background]),
        })
    }

    fn decode_color_ref(color: u32) -> (u8, u8, u8) {
        ((color & 0xff) as u8, ((color >> 8) & 0xff) as u8, ((color >> 16) & 0xff) as u8)
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use std::time::Duration;

    use super::DefaultColors;

    pub(super) fn detect_default_colors(_timeout: Duration) -> Option<DefaultColors> { None }
}
