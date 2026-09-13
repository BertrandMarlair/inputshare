//! Prints the monitors this machine actually has.
//!
//!     cargo run -p is-input --example displays

fn main() {
    match is_input::displays::enumerate() {
        Ok(displays) => {
            println!("{} display(s)", displays.len());
            for display in displays {
                println!(
                    "  {:<16} {:>5}x{:<5} at {:>6},{:<6}  scale {:.2}{}",
                    display.name,
                    display.width,
                    display.height,
                    display.x,
                    display.y,
                    display.scale,
                    if display.primary { "  primary" } else { "" }
                );
            }
        }
        Err(error) => eprintln!("could not read the displays: {error}"),
    }
}
