fn main() {
    #[cfg(feature = "stm32f1xx-hal")]
    println!("cargo:rustc-link-search=memory.x");

    let hse = std::env::var("EXAMPLE_HSE");

    if let Ok(hse) = hse {
        if hse == "bypass" {
            println!("cargo:rustc-cfg=hse=\"bypass\"")
        } else if hse == "oscillator" {
            println!("cargo:rustc-cfg=hse=\"oscillator\"");
        } else if hse != "off" {
            panic!("Invalid EXAMPLE_HSE value. Allowed values: bypass, oscillator, off")
        }
    }

    let example_pins = std::env::var("EXAMPLE_PINS");

    if let Ok(pins) = example_pins {
        if pins == "nucleo" {
            println!("cargo:rustc-cfg=pins=\"nucleo\"")
        } else if pins != "default" {
            panic!("Invalid EXAMPLE_PINS value. Allowed values: nucleo, default");
        }
    }

    println!("cargo:rerun-if-env-changed=EXAMPLE_HSE");
    println!("cargo:rerun-if-env-changed=EXAMPLE_PINS");
}
