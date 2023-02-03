//! For build and run instructions, see README.md
//!
//! With Wireshark, you can see the ARP packets, which should look like this:
//! No.  Time        Source          Destination     Protocol    Length  Info
//! 1	0.000000000	Cetia_ad:be:ef	Broadcast	    ARP	        60	    Who has 10.0.0.2? Tell 10.0.0.10

#![no_std]
#![no_main]

use defmt_rtt as _;
use panic_probe as _;

use core::cell::RefCell;
use core::default::Default;
use cortex_m_rt::{entry, exception};

use cortex_m::interrupt::Mutex;
use stm32_eth::{
    dma::{RxDescriptor, TxDescriptor},
    stm32::{interrupt, CorePeripherals, Peripherals, SYST},
    Parts, MTU,
};

#[cfg(feature = "f-series")]
use stm32_eth::mac::{phy::BarePhy, Phy};

pub mod common;

use stm32_eth::dma::{RxDescriptorRing, TxDescriptorRing, TxError};

const PHY_ADDR: u8 = 0;

static TIME: Mutex<RefCell<usize>> = Mutex::new(RefCell::new(0));
static ETH_PENDING: Mutex<RefCell<bool>> = Mutex::new(RefCell::new(false));

/// On H7s, the ethernet DMA does not have access to the normal ram
/// so we must explicitly put them in SRAM.
#[cfg_attr(feature = "stm32h7xx-hal", link_section = ".sram1.eth")]
static mut TX_DESCRIPTORS: [TxDescriptor; 4] = [TxDescriptor::new(); 4];
#[cfg_attr(feature = "stm32h7xx-hal", link_section = ".sram1.eth")]
static mut TX_BUFFERS: [[u8; MTU + 2]; 4] = [[0u8; MTU + 2]; 4];
#[cfg_attr(feature = "stm32h7xx-hal", link_section = ".sram1.eth2")]
static mut RX_DESCRIPTORS: [RxDescriptor; 4] = [RxDescriptor::new(); 4];
#[cfg_attr(feature = "stm32h7xx-hal", link_section = ".sram1.eth2")]
static mut RX_BUFFERS: [[u8; MTU + 2]; 4] = [[0u8; MTU + 2]; 4];

#[entry]
fn main() -> ! {
    let p = Peripherals::take().unwrap();
    let mut cp = CorePeripherals::take().unwrap();

    let (clocks, gpio, ethernet) = common::setup_peripherals(p);

    setup_systick(&mut cp.SYST);

    defmt::info!("Enabling ethernet...");

    let (eth_pins, mdio, mdc, _) = common::setup_pins(gpio);

    let rx_ring = RxDescriptorRing::new(unsafe { &mut RX_DESCRIPTORS }, unsafe { &mut RX_BUFFERS });
    let tx_ring = TxDescriptorRing::new(unsafe { &mut TX_DESCRIPTORS }, unsafe { &mut TX_BUFFERS });

    let Parts {
        mut dma,
        mac,
        #[cfg(feature = "ptp")]
            ptp: _,
    } = stm32_eth::new(ethernet, rx_ring, tx_ring, clocks, eth_pins).unwrap();
    dma.enable_interrupt();

    let mut last_link_up = false;

    #[cfg(feature = "f-series")]
    let mut bare_phy = BarePhy::new(_mac.with_mii(mdio, mdc), PHY_ADDR, Default::default());

    loop {
        #[cfg(feature = "f-series")]
        let link_up = bare_phy.phy_link_up();

        #[cfg(feature = "stm32h7xx-hal")]
        let link_up = true;

        if link_up != last_link_up {
            if link_up {
                defmt::info!("Ethernet: link detected");
            } else {
                defmt::info!("Ethernet: no link detected");
            }
            last_link_up = link_up;
        }

        if link_up {
            const SIZE: usize = 42;

            const DST_MAC: [u8; 6] = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];
            const SRC_MAC: [u8; 6] = [0x00, 0x00, 0xDE, 0xAD, 0xBE, 0xEF];
            const ETH_TYPE: [u8; 2] = [0x08, 0x06]; // ARP
            const HTYPE: [u8; 2] = [0x00, 0x01]; // Hardware Type: ethernet
            const PTYPE: [u8; 2] = [0x08, 0x00]; // IP
            const HLEN: [u8; 1] = [0x06]; // MAC length
            const PLEN: [u8; 1] = [0x04]; // IPv4
            const OPER: [u8; 2] = [0x00, 0x01]; // Operation: request
            const SENDER_IP: [u8; 4] = [0x0A, 00, 0x00, 0x0A]; // 10.0.0.10
            const TARGET_MAC: [u8; 6] = [0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
            const TARGET_IP: [u8; 4] = [0x0A, 0x00, 0x00, 0x02]; // 10.0.0.2

            let r = dma.send(SIZE, None, |buf| {
                buf[0..6].copy_from_slice(&DST_MAC);
                buf[6..12].copy_from_slice(&SRC_MAC);
                buf[12..14].copy_from_slice(&ETH_TYPE);
                buf[14..16].copy_from_slice(&HTYPE);

                buf[16..18].copy_from_slice(&PTYPE);
                buf[18..19].copy_from_slice(&HLEN);
                buf[19..20].copy_from_slice(&PLEN);
                buf[20..22].copy_from_slice(&OPER);
                buf[22..28].copy_from_slice(&SRC_MAC);
                buf[28..32].copy_from_slice(&SENDER_IP);

                buf[32..38].copy_from_slice(&TARGET_MAC);
                buf[38..42].copy_from_slice(&TARGET_IP);
            });

            loop {
                use core::hash::{Hash, Hasher};

                if let Ok(rx_packet) = dma.recv_next(None) {
                    let mut hasher = siphasher::sip::SipHasher::new();
                    rx_packet.hash(&mut hasher);

                    defmt::info!(
                        "Received {} bytes. Hash: {:016X}",
                        rx_packet.len(),
                        hasher.finish()
                    );
                    break;
                }
            }

            match r {
                Ok(()) => {
                    defmt::info!("ARP sent");
                }
                Err(TxError::WouldBlock) => {
                    defmt::info!("ARP failed. {}", dma.tx_state())
                }
            }
        } else {
            // defmt::info!("Down");
        }

        cortex_m::interrupt::free(|cs| {
            let mut eth_pending = ETH_PENDING.borrow(cs).borrow_mut();
            *eth_pending = false;
        });
    }
}

fn setup_systick(syst: &mut SYST) {
    syst.set_reload(100 * SYST::get_ticks_per_10ms());
    syst.enable_counter();
    syst.enable_interrupt();
}

#[exception]
fn SysTick() {
    cortex_m::interrupt::free(|cs| {
        let mut time = TIME.borrow(cs).borrow_mut();
        *time += 1;
    })
}

#[interrupt]
fn ETH() {
    cortex_m::interrupt::free(|cs| {
        let mut eth_pending = ETH_PENDING.borrow(cs).borrow_mut();
        *eth_pending = true;
    });

    // Clear interrupt flags
    let p = unsafe { Peripherals::steal() };
    stm32_eth::eth_interrupt_handler(&p.ETHERNET_DMA);
}
