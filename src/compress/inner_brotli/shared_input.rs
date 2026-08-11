use crate::compress::InputBuffer;
use simd_brotli::SliceWrapper;

/// Owned input for [`CompressMulti`], which shares the buffer across section
/// tasks and therefore cannot borrow it. Newtype because the orphan rule
/// forbids implementing brotli's `SliceWrapper` for [`InputBuffer`] directly.
pub struct SharedInput(pub InputBuffer);

impl SliceWrapper<u8> for SharedInput {
    fn slice(&self) -> &[u8] {
        self.0.as_ref()
    }
}
