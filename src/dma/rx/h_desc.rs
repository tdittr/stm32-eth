use core::sync::atomic::{self, Ordering};

use crate::dma::{raw_descriptor::RawDescriptor, PacketId, RxError};

#[cfg(feature = "ptp")]
use crate::ptp::Timestamp;

mod consts {
    #![allow(unused)]

    /// Owned by DMA
    pub const RXDESC_3_OWN: u32 = 1 << 31;

    // Read format bits
    /// Interrupt On Completion
    pub const RXDESC_3_IOC: u32 = 1 << 30;
    /// Buffer 2 Address Valid
    pub const RXDESC_3_BUF2V: u32 = 1 << 25;
    /// Buffer 1 Address valid
    pub const RXDESC_3_BUF1V: u32 = 1 << 24;

    // Write-back bits
    /// Context Descriptor
    pub const RXDESC_3_CTXT: u32 = 1 << 30;
    /// First Descriptor
    pub const RXDESC_3_FD: u32 = 1 << 29;
    /// Last Descriptor
    pub const RXDESC_3_LD: u32 = 1 << 28;
    /// Receive Status RDES2 valid
    pub const RXDESC_3_RS2V: u32 = 1 << 27;
    /// Receive status RDES1 valid
    pub const RXDESC_3_RS1V: u32 = 1 << 26;
    /// Receive status RDES0 valid
    pub const RXDESC_3_RS0V: u32 = 1 << 26;
    /// CRC error
    pub const RXDESC_3_CE: u32 = 1 << 24;
    /// Giant Packet
    pub const RXDESC_3_GP: u32 = 1 << 23;
    /// Receive Watchdog Timeout
    pub const RXDESC_3_RWT: u32 = 1 << 22;
    /// Overflow Error
    pub const RXDESC_3_OE: u32 = 1 << 21;
    /// Receive Error
    pub const RXDESC_3_RE: u32 = 1 << 20;
    /// Dribble Bit Error
    pub const RXDESC_3_DE: u32 = 1 << 19;

    /// Length/Type Field shift
    pub const RXDESC_3_LT_SHIFT: u32 = 16;
    /// Length/Type Field mask
    pub const RXDESC_3_LT_MASK: u32 = 0b111 << RXDESC_3_LT_SHIFT;
    /// Length/Type Field
    #[allow(non_camel_case_types)]
    #[repr(u8)]
    pub enum RXDESC_3_LT {
        Length = 0b000,
        Type = 0b001,
        Reserved = 0b010,
        ArpRequest = 0b011,
        TypeWithVlan = 0b100,
        TypeWIthDoubleVlan = 0b101,
        MacControl = 0b110,
        Oam = 0b111,
    }

    /// Error Summary
    pub const RXDESC_3_ES: u32 = 1 << 15;

    /// Packet Length shift
    pub const RXDESC_3_PL_SHIFT: u32 = 0;
    /// Packet Length mask
    pub const RXDESC_3_PL_MASK: u32 = 0x3FFF;
}
pub use consts::*;

#[repr(C)]
#[repr(align(4))]
#[derive(Clone, Copy)]
/// An RX DMA Descriptor.
pub struct RxDescriptor {
    inner_raw: RawDescriptor,
    packet_id: Option<PacketId>,
    #[cfg(feature = "ptp")]
    cached_timestamp: Option<Timestamp>,
}

impl Default for RxDescriptor {
    fn default() -> Self {
        Self::new()
    }
}

impl RxDescriptor {
    /// Creates a new [`RxDescriptor`].
    pub const fn new() -> Self {
        Self {
            inner_raw: RawDescriptor::new(),
            packet_id: None,
            #[cfg(feature = "ptp")]
            cached_timestamp: None,
        }
    }

    pub(super) fn setup(&mut self, buffer: &[u8]) {
        self.set_owned(buffer.as_ptr());
    }

    /// Is owned by the DMA engine?
    fn is_owned(&self) -> bool {
        (self.inner_raw.read(3) & RXDESC_3_OWN) == RXDESC_3_OWN
    }

    /// Pass ownership to the DMA engine
    pub(super) fn set_owned(&mut self, buffer: *const u8) {
        self.set_buffer(buffer);

        // "Preceding reads and writes cannot be moved past subsequent writes."
        #[cfg(feature = "fence")]
        atomic::fence(Ordering::Release);
        atomic::compiler_fence(Ordering::Release);

        unsafe {
            self.inner_raw
                .modify(3, |w| w | RXDESC_3_OWN | RXDESC_3_IOC);
        }

        cortex_m::asm::dsb();

        assert!(self.is_owned());

        // Used to flush the store buffer as fast as possible to make the buffer available for the
        // DMA.
        #[cfg(feature = "fence")]
        atomic::fence(Ordering::SeqCst);
    }

    /// Configure the buffer and its length.
    fn set_buffer(&mut self, buffer_ptr: *const u8) {
        unsafe {
            // Set buffer 1 address.
            self.inner_raw.modify(0, |_| buffer_ptr as u32);

            // RXDESC does not contain buffer length, it is set
            // in register INSERT_HERE instead. The size of all
            // buffers is verified by [`TxRing`](super::TxRing)

            self.inner_raw.modify(3, |w| {
                // BUF2 is not valid
                let w = w & !(RXDESC_3_BUF2V);
                // BUF1 is valid
                let w = w | RXDESC_3_BUF1V;
                w
            });
        }
    }

    fn has_error(&self) -> bool {
        self.inner_raw.read(3) & RXDESC_3_ES == RXDESC_3_ES
    }

    fn is_first(&self) -> bool {
        self.inner_raw.read(3) & RXDESC_3_FD == RXDESC_3_FD
    }

    fn is_last(&self) -> bool {
        self.inner_raw.read(3) & RXDESC_3_LD == RXDESC_3_LD
    }

    fn is_context(&self) -> bool {
        self.inner_raw.read(3) & RXDESC_3_CTXT == RXDESC_3_CTXT
    }

    pub(super) fn take_received(
        &mut self,
        // NOTE(allow): packet_id is unused if ptp is disabled.
        #[allow(unused_variables)] packet_id: Option<PacketId>,
        buffer: &mut [u8],
    ) -> Result<(), RxError> {
        if self.is_owned() {
            Err(RxError::WouldBlock)
        } else
        // Only single-frame descriptors and non-context descriptors are supported
        // for now.
        if self.is_first() && self.is_last() && !self.has_error() && !self.is_context() {
            // "Subsequent reads and writes cannot be moved ahead of preceding reads."
            atomic::compiler_fence(Ordering::Acquire);

            self.packet_id = packet_id;

            // Cache the PTP timestamps if PTP is enabled.
            #[cfg(feature = "ptp")]
            self.attach_timestamp();

            Ok(())
        } else {
            self.set_owned(buffer.as_ptr());
            Err(RxError::Truncated)
        }
    }

    pub(super) fn frame_length(&self) -> usize {
        if self.is_owned() {
            0
        } else {
            ((self.inner_raw.read(3) & RXDESC_3_PL_MASK) >> RXDESC_3_PL_SHIFT) as usize
        }
    }

    #[allow(unused)]
    pub(super) fn packet_id(&self) -> Option<&PacketId> {
        self.packet_id.as_ref()
    }
}

#[cfg(feature = "ptp")]
impl RxDescriptor {
    /// Get PTP timestamps if available
    pub(super) fn read_timestamp(&self) -> Option<Timestamp> {
        #[cfg(not(feature = "stm32f1xx-hal"))]
        let is_valid = {
            /// RX timestamp
            const RXDESC_0_TIMESTAMP_VALID: u32 = 1 << 7;
            self.inner_raw.read(0) & RXDESC_0_TIMESTAMP_VALID == RXDESC_0_TIMESTAMP_VALID
        };

        #[cfg(feature = "stm32f1xx-hal")]
        // There is no "timestamp valid" indicator bit
        // on STM32F1XX
        let is_valid = true;

        let timestamp = Timestamp::from_descriptor(&self.inner_raw);

        if is_valid && self.is_last() {
            timestamp
        } else {
            None
        }
    }

    fn attach_timestamp(&mut self) {
        self.cached_timestamp = self.read_timestamp();
    }

    pub(super) fn timestamp(&self) -> Option<&Timestamp> {
        self.cached_timestamp.as_ref()
    }
}
