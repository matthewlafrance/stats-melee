// DTM header encoder for Dolphin's native input-replay format.
// Pure encoder — no IO. See docs/slp-to-dtm-render.md for the full spec.
//
// The plan doc had `video_backend` as 32 bytes; the real Dolphin struct uses
// 16, making the correct header exactly 256 bytes (0x100). All offsets from
// 0x051 onward are 16 less than what the plan doc listed.

/// Total size of the DTM header in bytes.
pub const HEADER_SIZE: usize = 256;

/// Size of one per-port controller record in bytes.
pub const FRAME_SIZE: usize = 8;

const DTM_SIGNATURE: [u8; 4] = [0x44, 0x54, 0x4D, 0x1A]; // "DTM\x1a"

/// NTSC Melee 1.02 game ID.
pub const GAME_ID_GALE01: [u8; 6] = *b"GALE01";

/// DTM file header (256 bytes). All integers are little-endian; strings are
/// NUL-padded. Byte layout verified against Dolphin's `Movie.h` struct.
#[derive(Debug, Clone)]
pub struct DtmHeader {
    /// 6-byte game ID. Use [`GAME_ID_GALE01`] for NTSC Melee 1.02.
    pub game_id: [u8; 6],
    pub is_wii: bool,
    /// Bitmask: bit 0 = port 1, bit 1 = port 2, etc. 1v1: `0b00000011`.
    pub controllers_plugged: u8,
    pub starts_from_savestate: bool,
    /// Total VBlank frames. Must equal `input_count`.
    pub vi_count: u64,
    /// Total controller-poll frames. Must equal `vi_count`.
    pub input_count: u64,
    pub lag_counter: u64,
    pub rerecord_count: u32,
    /// Author string, NUL-padded to 32 bytes.
    pub author: [u8; 32],
    /// Video backend string, NUL-padded to 16 bytes. Empty for our use.
    pub video_backend: [u8; 16],
    /// Raw 16-byte MD5 of the Melee ISO.
    pub game_md5: [u8; 16],
    pub recording_start_time: u64,
    // ── Config block (0x089–0x09D) ────────────────────────────────────────────
    // Honored by Dolphin when `saved_config_valid` is true.
    pub saved_config_valid: bool,
    pub idle_skipping: bool,
    pub dual_core: bool,
    pub progressive_scan: bool,
    pub dsp_hle: bool,
    pub fast_disc_speed: bool,
    /// CPU core type. 1 = JIT recompiler.
    pub cpu_core: u8,
    pub efb_access: bool,
    pub efb_copy: bool,
    pub copy_efb_to_texture: bool,
    pub efb_copy_cache: bool,
    pub emulate_format_changes: bool,
    pub use_xfb: bool,
    pub use_real_xfb: bool,
    pub memory_cards_present: u8,
    pub memory_card_blank: bool,
    pub bongos_plugged: u8,
    pub sync_gpu_thread: bool,
    pub netplay_session: bool,
    pub sysconf_pal60: bool,
    pub language: u8,
    // ── Extended config (0x09F–0x0A3, within the 14-byte reserved block) ─────
    pub jit_branch_following: bool,
    pub accurate_fma: bool,
    pub gbas_plugged: u8,
    pub sysconf_widescreen: bool,
    /// SYSCONF country code. 0x31 = US.
    pub sysconf_country: u8,
    /// CPU tick count at end of recording.  Dolphin's `CheckInputEnd` calls
    /// `EndPlayInput` when `GetTicks() > tick_count` (and we're not playing
    /// from a savestate), so leaving this at 0 ends playback after the very
    /// first poll.  Set to `u64::MAX` so the tick check never triggers.
    pub tick_count: u64,
}

impl DtmHeader {
    /// Construct a header with default config values for NTSC Melee 1.02
    /// running in vanilla Dolphin. `vi_count` is set equal to `input_count`.
    pub fn new_for_melee(
        input_count: u64,
        game_md5: [u8; 16],
        recording_start_time: u64,
    ) -> Self {
        // Author must be zero-padded — Dolphin's own recordings leave it
        // empty, and writing a tag here is a free byte-level diff from a
        // known-good reference.
        let author = [0u8; 32];

        // videoBackend = "Metal" matches what Dolphin writes on Apple Silicon.
        // Empty here causes Dolphin's movie loader to crash trying to switch
        // to a nameless backend when saved_config_valid is true.
        let mut video_backend = [0u8; 16];
        video_backend[..5].copy_from_slice(b"Metal");

        Self {
            game_id: GAME_ID_GALE01,
            is_wii: false,
            controllers_plugged: 0b00000011,
            starts_from_savestate: false,
            // Dolphin records inputCount = 2 × frameCount because Melee polls
            // the Serial Interface twice per game frame. We pass the doubled
            // count through input_count and keep frameCount (vi_count) at the
            // game-frame count. Caller is responsible for providing 2× the
            // controller records to match.
            vi_count: input_count / 2,
            input_count,
            lag_counter: 0,
            rerecord_count: 0,
            author,
            video_backend,
            game_md5,
            recording_start_time,
            // saved_config_valid=false → Dolphin uses its own settings and
            // skips the mid-boot config mutation that trips a double-free
            // bug in JitArm64::ResetFreeMemoryRanges on macOS / Apple Silicon
            // (Dolphin 2603a). Symbols caught it: `___BUG_IN_CLIENT_OF_LIBMALLOC`
            // → ResetFreeMemoryRanges → ClearCache → CBoot::RunApploader.
            // The config bytes below are still populated for byte-level
            // diffability against a Dolphin-recorded reference, but Dolphin
            // ignores them when this flag is false.
            saved_config_valid: false,
            idle_skipping: true,
            dual_core: false,
            progressive_scan: false,
            dsp_hle: true,
            fast_disc_speed: false,
            cpu_core: 4, // JITARM64 — required on Apple Silicon
            efb_access: false,
            efb_copy: true,
            copy_efb_to_texture: true,
            efb_copy_cache: false,
            emulate_format_changes: false,
            use_xfb: false,
            use_real_xfb: true,
            memory_cards_present: 1,
            memory_card_blank: false,
            bongos_plugged: 0,
            sync_gpu_thread: false,
            netplay_session: false,
            sysconf_pal60: true,
            language: 0,
            jit_branch_following: true,
            accurate_fma: true,
            gbas_plugged: 0,
            sysconf_widescreen: true,
            sysconf_country: 0x6c,
            // u64::MAX disables Dolphin's tick-based end-of-playback check.
            tick_count: u64::MAX,
        }
    }

    /// Serialize to exactly `HEADER_SIZE` (256) bytes.
    /// Reserved and zero-only fields are always zero.
    pub fn serialize(&self) -> [u8; HEADER_SIZE] {
        let mut b = [0u8; HEADER_SIZE];

        b[0x000..0x004].copy_from_slice(&DTM_SIGNATURE);        // "DTM\x1a"
        b[0x004..0x00A].copy_from_slice(&self.game_id);
        b[0x00A] = self.is_wii as u8;
        b[0x00B] = self.controllers_plugged;
        b[0x00C] = self.starts_from_savestate as u8;
        b[0x00D..0x015].copy_from_slice(&self.vi_count.to_le_bytes());
        b[0x015..0x01D].copy_from_slice(&self.input_count.to_le_bytes());
        b[0x01D..0x025].copy_from_slice(&self.lag_counter.to_le_bytes());
        // 0x025..0x02D: uniqueID / reserved (8 bytes, zero)
        b[0x02D..0x031].copy_from_slice(&self.rerecord_count.to_le_bytes());
        b[0x031..0x051].copy_from_slice(&self.author);
        b[0x051..0x061].copy_from_slice(&self.video_backend);   // 16 bytes
        // 0x061..0x071: audioEmulator (16 bytes, zero)
        b[0x071..0x081].copy_from_slice(&self.game_md5);
        b[0x081..0x089].copy_from_slice(&self.recording_start_time.to_le_bytes());
        // ── Config block ──────────────────────────────────────────────────────
        b[0x089] = self.saved_config_valid as u8;
        b[0x08A] = self.idle_skipping as u8;
        b[0x08B] = self.dual_core as u8;
        b[0x08C] = self.progressive_scan as u8;
        b[0x08D] = self.dsp_hle as u8;
        b[0x08E] = self.fast_disc_speed as u8;
        b[0x08F] = self.cpu_core;
        b[0x090] = self.efb_access as u8;
        b[0x091] = self.efb_copy as u8;
        b[0x092] = self.copy_efb_to_texture as u8;
        b[0x093] = self.efb_copy_cache as u8;
        b[0x094] = self.emulate_format_changes as u8;
        b[0x095] = self.use_xfb as u8;
        b[0x096] = self.use_real_xfb as u8;
        b[0x097] = self.memory_cards_present;
        b[0x098] = self.memory_card_blank as u8;
        b[0x099] = self.bongos_plugged;
        b[0x09A] = self.sync_gpu_thread as u8;
        b[0x09B] = self.netplay_session as u8;
        b[0x09C] = self.sysconf_pal60 as u8;
        b[0x09D] = self.language;
        // ── 14-byte reserved block (0x09E–0x0AB) ──────────────────────────────
        // 0x09E: reserved (zero)
        b[0x09F] = self.jit_branch_following as u8;
        b[0x0A0] = self.accurate_fma as u8;
        b[0x0A1] = self.gbas_plugged;
        b[0x0A2] = self.sysconf_widescreen as u8;
        b[0x0A3] = self.sysconf_country;
        // 0x0A4..0x0AB: reserved (zero)
        // 0x0AC..0x0D4: discChange / second disc ISO (40 bytes, zero)
        // 0x0D4..0x0E8: Dolphin git revision (20 bytes, zero)
        // 0x0E8..0x0EC: DSP IROM hash (4 bytes, zero)
        // 0x0EC..0x0F0: DSP COEF hash (4 bytes, zero)
        b[0x0F0..0x0F8].copy_from_slice(&self.tick_count.to_le_bytes());
        // 0x0F8..0x100: reserved (8 bytes, zero)

        b
    }
}

/// Per-port GameCube controller state for one frame (8 bytes when serialized).
#[derive(Debug, Clone, Default)]
pub struct DtmControllerState {
    // ── Byte 0 ───────────────────────────────────────────────────────────────
    pub start: bool,
    pub a: bool,
    pub b: bool,
    pub x: bool,
    pub y: bool,
    pub z: bool,
    pub dpad_up: bool,
    pub dpad_down: bool,
    // ── Byte 1 ───────────────────────────────────────────────────────────────
    pub dpad_left: bool,
    pub dpad_right: bool,
    pub l_digital: bool,
    pub r_digital: bool,
    // change_disc (bit 4), reset (bit 5), reset_analog (bit 7) are always 0.
    // connected (bit 6) is always 1 — written by serialize(), not stored here.
    // ── Bytes 2–7 ────────────────────────────────────────────────────────────
    pub l_pressure: u8,
    pub r_pressure: u8,
    /// 0–255, center 128.
    pub stick_x: u8,
    /// 0–255, center 128.
    pub stick_y: u8,
    /// 0–255, center 128.
    pub cstick_x: u8,
    /// 0–255, center 128.
    pub cstick_y: u8,
}

impl DtmControllerState {
    /// Neutral input: no buttons, sticks centered, triggers at rest.
    pub fn neutral() -> Self {
        Self {
            stick_x: 128,
            stick_y: 128,
            cstick_x: 128,
            cstick_y: 128,
            ..Default::default()
        }
    }

    /// Encode to 8 bytes. The `connected` bit (byte 1, bit 6) is always set.
    pub fn serialize(&self) -> [u8; FRAME_SIZE] {
        let byte0 = (self.start as u8)
            | ((self.a as u8) << 1)
            | ((self.b as u8) << 2)
            | ((self.x as u8) << 3)
            | ((self.y as u8) << 4)
            | ((self.z as u8) << 5)
            | ((self.dpad_up as u8) << 6)
            | ((self.dpad_down as u8) << 7);

        let byte1 = (self.dpad_left as u8)
            | ((self.dpad_right as u8) << 1)
            | ((self.l_digital as u8) << 2)
            | ((self.r_digital as u8) << 3)
            | (1u8 << 6); // connected — always 1

        [
            byte0,
            byte1,
            self.l_pressure,
            self.r_pressure,
            self.stick_x,
            self.stick_y,
            self.cstick_x,
            self.cstick_y,
        ]
    }
}

/// Assemble a complete DTM file: header + interleaved per-frame controller records.
///
/// `frames[i]` holds one `DtmControllerState` per active port for frame `i`,
/// in ascending port order. The caller must ensure
/// `header.input_count == frames.len() as u64`.
pub fn write_dtm(header: &DtmHeader, frames: &[Vec<DtmControllerState>]) -> Vec<u8> {
    let ports_per_frame = frames.first().map(|f| f.len()).unwrap_or(0);
    let mut out = Vec::with_capacity(
        HEADER_SIZE + frames.len() * ports_per_frame * FRAME_SIZE,
    );
    out.extend_from_slice(&header.serialize());
    for frame in frames {
        for ctrl in frame {
            out.extend_from_slice(&ctrl.serialize());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const NTSC_102_MD5: [u8; 16] = [
        0x0e, 0x63, 0xd4, 0x22, 0x3b, 0x01, 0xd9, 0xab,
        0xa5, 0x96, 0x25, 0x9d, 0xc1, 0x55, 0xa1, 0x74,
    ];

    fn fixture() -> DtmHeader {
        DtmHeader::new_for_melee(120, NTSC_102_MD5, 1_700_000_000)
    }

    // ── Header layout ────────────────────────────────────────────────────────

    #[test]
    fn header_is_256_bytes() {
        assert_eq!(HEADER_SIZE, 256);
        assert_eq!(fixture().serialize().len(), 256);
    }

    #[test]
    fn signature_at_0x000() {
        let b = fixture().serialize();
        assert_eq!(&b[0x000..0x004], &[0x44, 0x54, 0x4D, 0x1A]);
    }

    #[test]
    fn game_id_gale01_at_0x004() {
        assert_eq!(&fixture().serialize()[0x004..0x00A], b"GALE01");
    }

    #[test]
    fn is_wii_zero_at_0x00a() {
        assert_eq!(fixture().serialize()[0x00A], 0);
    }

    #[test]
    fn controllers_plugged_ports_1_and_2() {
        assert_eq!(fixture().serialize()[0x00B], 0b00000011);
    }

    #[test]
    fn vi_count_is_half_input_count_little_endian() {
        // new_for_melee sets vi_count = input_count / 2 (Dolphin's recordings
        // show this 2:1 ratio because Melee polls the SI twice per game frame).
        let b = fixture().serialize();
        let vi = u64::from_le_bytes(b[0x00D..0x015].try_into().unwrap());
        let ic = u64::from_le_bytes(b[0x015..0x01D].try_into().unwrap());
        assert_eq!(ic, 120, "input_count");
        assert_eq!(vi, 60, "vi_count = input_count / 2");
    }

    #[test]
    fn lag_counter_zero_at_0x01d() {
        let b = fixture().serialize();
        assert_eq!(u64::from_le_bytes(b[0x01D..0x025].try_into().unwrap()), 0);
    }

    #[test]
    fn rerecord_count_zero_at_0x02d() {
        let b = fixture().serialize();
        assert_eq!(u32::from_le_bytes(b[0x02D..0x031].try_into().unwrap()), 0);
    }

    #[test]
    fn author_zeroed_to_match_dolphin_reference() {
        // Dolphin's GUI recorder leaves author empty; we follow suit so our
        // header byte-diffs cleanly against a reference DTM.
        assert!(fixture().serialize()[0x031..0x051].iter().all(|&x| x == 0));
    }

    #[test]
    fn video_backend_metal_at_0x051() {
        let b = fixture().serialize();
        assert_eq!(&b[0x051..0x056], b"Metal");
        assert!(b[0x056..0x061].iter().all(|&x| x == 0));
    }

    #[test]
    fn audio_emulator_zeroed_at_0x061() {
        let b = fixture().serialize();
        assert!(b[0x061..0x071].iter().all(|&x| x == 0));
    }

    #[test]
    fn game_md5_at_0x071() {
        assert_eq!(&fixture().serialize()[0x071..0x081], &NTSC_102_MD5);
    }

    #[test]
    fn recording_start_time_little_endian_at_0x081() {
        let ts = 1_700_000_000u64;
        let b = DtmHeader::new_for_melee(0, [0u8; 16], ts).serialize();
        let v = u64::from_le_bytes(b[0x081..0x089].try_into().unwrap());
        assert_eq!(v, ts);
    }

    #[test]
    fn config_block_matches_dolphin_reference_except_save_flag() {
        // Mirror of bytes 0x089..0x0AC from a Dolphin 2603a-recorded DTM,
        // EXCEPT saved_config_valid (0x089) which we deliberately keep at 0
        // to dodge the JitArm64 double-free bug. The remaining 34 bytes are
        // populated for byte-level diffability against a real DTM.
        let expected_after_flag: [u8; 34] = [
                  0x01, 0x00, 0x00, 0x01, 0x00, 0x04,             // 0x08A-0x08F
            0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x01, 0x01,       // 0x090-0x097
            0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x01,       // 0x098-0x09F
            0x01, 0x00, 0x01, 0x6c, 0x00, 0x00, 0x00, 0x00,       // 0x0A0-0x0A7
            0x00, 0x00, 0x00, 0x00,                               // 0x0A8-0x0AB
        ];
        let b = fixture().serialize();
        assert_eq!(b[0x089], 0x00, "saved_config_valid intentionally false");
        assert_eq!(&b[0x08A..0x0AC], &expected_after_flag[..]);
    }

    #[test]
    fn tick_count_is_max_to_disable_end_check() {
        // Dolphin's CheckInputEnd calls EndPlayInput when GetTicks() >
        // tick_count.  Leaving it at 0 ends playback on the first poll, so
        // we must write u64::MAX.
        let b = fixture().serialize();
        let tc = u64::from_le_bytes(b[0x0F0..0x0F8].try_into().unwrap());
        assert_eq!(tc, u64::MAX);
    }

    #[test]
    fn reserved_ranges_are_zero() {
        let b = fixture().serialize();
        assert!(b[0x025..0x02D].iter().all(|&x| x == 0), "uniqueID/reserved");
        assert!(b[0x0A4..0x0AC].iter().all(|&x| x == 0), "reserved tail of 14-byte block");
        assert!(b[0x0AC..0x0D4].iter().all(|&x| x == 0), "discChange");
        assert!(b[0x0D4..0x0E8].iter().all(|&x| x == 0), "dolphin revision");
        assert!(b[0x0E8..0x0EC].iter().all(|&x| x == 0), "dsp_irom_hash");
        assert!(b[0x0EC..0x0F0].iter().all(|&x| x == 0), "dsp_coef_hash");
        assert!(b[0x0F8..0x100].iter().all(|&x| x == 0), "final reserved");
    }

    // ── DtmControllerState ───────────────────────────────────────────────────

    #[test]
    fn neutral_sticks_at_128() {
        let b = DtmControllerState::neutral().serialize();
        assert_eq!(b[4], 128, "stick_x");
        assert_eq!(b[5], 128, "stick_y");
        assert_eq!(b[6], 128, "cstick_x");
        assert_eq!(b[7], 128, "cstick_y");
    }

    #[test]
    fn neutral_no_buttons_no_triggers() {
        let b = DtmControllerState::neutral().serialize();
        assert_eq!(b[0], 0, "byte0: no buttons");
        assert_eq!(b[2], 0, "l_pressure");
        assert_eq!(b[3], 0, "r_pressure");
    }

    #[test]
    fn connected_bit_always_set() {
        assert_ne!(DtmControllerState::neutral().serialize()[1] & (1 << 6), 0);
    }

    #[test]
    fn change_disc_reset_reset_analog_always_zero() {
        let b = DtmControllerState::neutral().serialize();
        assert_eq!(b[1] & (1 << 4), 0, "change_disc");
        assert_eq!(b[1] & (1 << 5), 0, "reset");
        assert_eq!(b[1] & (1 << 7), 0, "reset_analog");
    }

    #[test]
    fn byte0_button_positions() {
        let check = |field: &str, bit: u8, f: fn(&mut DtmControllerState)| {
            let mut c = DtmControllerState::default();
            f(&mut c);
            assert_eq!(c.serialize()[0], 1u8 << bit, "{field}");
        };
        check("start",     0, |c| c.start     = true);
        check("a",         1, |c| c.a         = true);
        check("b",         2, |c| c.b         = true);
        check("x",         3, |c| c.x         = true);
        check("y",         4, |c| c.y         = true);
        check("z",         5, |c| c.z         = true);
        check("dpad_up",   6, |c| c.dpad_up   = true);
        check("dpad_down", 7, |c| c.dpad_down = true);
    }

    #[test]
    fn byte1_button_positions() {
        let check = |field: &str, bit: u8, f: fn(&mut DtmControllerState)| {
            let mut c = DtmControllerState::default();
            f(&mut c);
            let b1 = c.serialize()[1];
            assert_ne!(b1 & (1 << bit), 0, "{field}");
            assert_ne!(b1 & (1 << 6),   0, "connected always set ({field})");
        };
        check("dpad_left",  0, |c| c.dpad_left  = true);
        check("dpad_right", 1, |c| c.dpad_right = true);
        check("l_digital",  2, |c| c.l_digital  = true);
        check("r_digital",  3, |c| c.r_digital  = true);
    }

    #[test]
    fn l_pressure_at_byte2_r_pressure_at_byte3() {
        let mut c = DtmControllerState::default();
        c.l_pressure = 200;
        c.r_pressure = 77;
        let b = c.serialize();
        assert_eq!(b[2], 200);
        assert_eq!(b[3], 77);
    }

    #[test]
    fn all_buttons_byte0_full() {
        let c = DtmControllerState {
            start: true, a: true, b: true, x: true,
            y: true, z: true, dpad_up: true, dpad_down: true,
            ..Default::default()
        };
        assert_eq!(c.serialize()[0], 0xFF);
    }

    // ── write_dtm ────────────────────────────────────────────────────────────

    #[test]
    fn write_dtm_total_size_1v1() {
        let n = 10usize;
        let header = DtmHeader::new_for_melee(n as u64, [0u8; 16], 0);
        let frames: Vec<Vec<DtmControllerState>> = (0..n)
            .map(|_| vec![DtmControllerState::neutral(), DtmControllerState::neutral()])
            .collect();
        let dtm = write_dtm(&header, &frames);
        assert_eq!(dtm.len(), HEADER_SIZE + n * 2 * FRAME_SIZE);
    }

    #[test]
    fn write_dtm_starts_with_signature() {
        let header = DtmHeader::new_for_melee(1, [0u8; 16], 0);
        let dtm = write_dtm(&header, &[vec![DtmControllerState::neutral()]]);
        assert_eq!(&dtm[0..4], &[0x44, 0x54, 0x4D, 0x1A]);
    }

    #[test]
    fn write_dtm_interleaves_ports_per_frame() {
        let header = DtmHeader::new_for_melee(1, [0u8; 16], 0);
        let mut p1 = DtmControllerState::default();
        p1.a = true;
        let mut p2 = DtmControllerState::default();
        p2.b = true;
        let dtm = write_dtm(&header, &[vec![p1, p2]]);
        assert_eq!(dtm[HEADER_SIZE] & 0b00000010, 0b00000010, "port1 A");
        assert_eq!(dtm[HEADER_SIZE + 8] & 0b00000100, 0b00000100, "port2 B");
    }

    #[test]
    fn write_dtm_two_frames_sequential() {
        let header = DtmHeader::new_for_melee(2, [0u8; 16], 0);
        let mut f0 = DtmControllerState::default();
        f0.start = true;
        let mut f1 = DtmControllerState::default();
        f1.z = true;
        let dtm = write_dtm(&header, &[vec![f0], vec![f1]]);
        assert_eq!(dtm.len(), HEADER_SIZE + 2 * FRAME_SIZE);
        assert_eq!(dtm[HEADER_SIZE] & 0b00000001, 1, "frame0 start");
        assert_eq!(dtm[HEADER_SIZE + 8] & 0b00100000, 0b00100000, "frame1 Z");
    }

    #[test]
    fn write_dtm_empty_frames_is_just_header() {
        let header = DtmHeader::new_for_melee(0, [0u8; 16], 0);
        assert_eq!(write_dtm(&header, &[]).len(), HEADER_SIZE);
    }
}
