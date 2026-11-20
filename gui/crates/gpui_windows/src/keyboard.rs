use anyhow::Result;
use collections::HashMap;
use windows::Win32::UI::{
    Input::KeyboardAndMouse::{
        GetKeyboardLayout, GetKeyboardLayoutNameW, MAPVK_VK_TO_CHAR, MAPVK_VK_TO_VSC,
        MAPVK_VSC_TO_VK_EX, MapVirtualKeyExW, MapVirtualKeyW, ToUnicode, ToUnicodeEx, VIRTUAL_KEY,
        VK_0, VK_1, VK_2, VK_3, VK_4, VK_5, VK_6, VK_7, VK_8, VK_9, VK_ABNT_C1, VK_CAPITAL,
        VK_CONTROL, VK_MENU, VK_OEM_1, VK_OEM_2, VK_OEM_3, VK_OEM_4, VK_OEM_5, VK_OEM_6, VK_OEM_7,
        VK_OEM_8, VK_OEM_102, VK_OEM_COMMA, VK_OEM_MINUS, VK_OEM_PERIOD, VK_OEM_PLUS, VK_SHIFT,
    },
    WindowsAndMessaging::KL_NAMELENGTH,
};

use gpui::{
    KeybindingKeystroke, Keystroke, Modifiers, PlatformKeyboardLayout, PlatformKeyboardMapper,
};

pub(crate) struct WindowsKeyboardLayout {
    id: String,
    name: String,
    keys: Vec<gpui::KeyboardCharacterKey>,
}

pub(crate) struct WindowsKeyboardMapper {
    key_to_vkey: HashMap<String, (u16, bool)>,
    vkey_to_key: HashMap<u16, String>,
    vkey_to_shifted: HashMap<u16, String>,
}

impl PlatformKeyboardLayout for WindowsKeyboardLayout {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn character_keys(&self) -> Vec<gpui::KeyboardCharacterKey> {
        self.keys.clone()
    }
}

impl PlatformKeyboardMapper for WindowsKeyboardMapper {
    fn map_key_equivalent(
        &self,
        mut keystroke: Keystroke,
        use_key_equivalents: bool,
    ) -> KeybindingKeystroke {
        let Some((vkey, shifted_key)) = self.get_vkey_from_key(&keystroke.key, use_key_equivalents)
        else {
            return KeybindingKeystroke::from_keystroke(keystroke);
        };
        if shifted_key && keystroke.modifiers.shift {
            log::warn!(
                "Keystroke '{}' has both shift and a shifted key, this is likely a bug",
                keystroke.key
            );
        }

        let shift = shifted_key || keystroke.modifiers.shift;
        keystroke.modifiers.shift = false;

        let Some(key) = self.vkey_to_key.get(&vkey).cloned() else {
            log::error!(
                "Failed to map key equivalent '{:?}' to a valid key",
                keystroke
            );
            return KeybindingKeystroke::from_keystroke(keystroke);
        };

        keystroke.key = if shift {
            let Some(shifted_key) = self.vkey_to_shifted.get(&vkey).cloned() else {
                log::error!(
                    "Failed to map keystroke {:?} with virtual key '{:?}' to a shifted key",
                    keystroke,
                    vkey
                );
                return KeybindingKeystroke::from_keystroke(keystroke);
            };
            shifted_key
        } else {
            key.clone()
        };

        let modifiers = Modifiers {
            shift,
            ..keystroke.modifiers
        };

        KeybindingKeystroke::new(keystroke, modifiers, key)
    }

    fn get_key_equivalents(&self) -> Option<&HashMap<char, char>> {
        None
    }
}

impl WindowsKeyboardLayout {
    pub(crate) fn new() -> Result<Self> {
        let mut buffer = [0u16; KL_NAMELENGTH as usize]; // KL_NAMELENGTH includes the null terminator
        unsafe { GetKeyboardLayoutNameW(&mut buffer)? };
        let id = String::from_utf16_lossy(&buffer[..buffer.len() - 1]); // Remove the null terminator
        let entry = windows_registry::LOCAL_MACHINE.open(format!(
            "System\\CurrentControlSet\\Control\\Keyboard Layouts\\{id}"
        ))?;
        let name = entry.get_string("Layout Text")?;
        Ok(Self {
            id,
            name,
            keys: character_keys(),
        })
    }

    pub(crate) fn unknown() -> Self {
        Self {
            id: "unknown".to_string(),
            name: "unknown".to_string(),
            keys: Vec::new(),
        }
    }
}

// Set-1 scan codes for printable USB keyboard positions. Extended navigation
// keys are deliberately absent; the same table samples layouts and tags input.
const CHARACTER_POSITIONS: &[(u16, u16)] = &[
    (0x1e, 0x04),
    (0x30, 0x05),
    (0x2e, 0x06),
    (0x20, 0x07),
    (0x12, 0x08),
    (0x21, 0x09),
    (0x22, 0x0a),
    (0x23, 0x0b),
    (0x17, 0x0c),
    (0x24, 0x0d),
    (0x25, 0x0e),
    (0x26, 0x0f),
    (0x32, 0x10),
    (0x31, 0x11),
    (0x18, 0x12),
    (0x19, 0x13),
    (0x10, 0x14),
    (0x13, 0x15),
    (0x1f, 0x16),
    (0x14, 0x17),
    (0x16, 0x18),
    (0x2f, 0x19),
    (0x11, 0x1a),
    (0x2d, 0x1b),
    (0x15, 0x1c),
    (0x2c, 0x1d),
    (0x02, 0x1e),
    (0x03, 0x1f),
    (0x04, 0x20),
    (0x05, 0x21),
    (0x06, 0x22),
    (0x07, 0x23),
    (0x08, 0x24),
    (0x09, 0x25),
    (0x0a, 0x26),
    (0x0b, 0x27),
    (0x39, 0x2c),
    (0x0c, 0x2d),
    (0x0d, 0x2e),
    (0x1a, 0x2f),
    (0x1b, 0x30),
    (0x2b, 0x31),
    (0x27, 0x33),
    (0x28, 0x34),
    (0x29, 0x35),
    (0x33, 0x36),
    (0x34, 0x37),
    (0x35, 0x38),
    (0x56, 0x64),
];

pub(crate) fn character_usage(scan: u16) -> Option<u16> {
    CHARACTER_POSITIONS
        .iter()
        .find_map(|&(code, usage)| (code == scan).then_some(usage))
}

fn character_keys() -> Vec<gpui::KeyboardCharacterKey> {
    // SAFETY: zero selects this GUI thread's current layout; the handle is borrowed.
    let layout = unsafe { GetKeyboardLayout(0) };
    CHARACTER_POSITIONS
        .iter()
        .map(|&(scan, usage)| {
            // SAFETY: mapping a scalar scan code does not mutate keyboard state.
            let vkey =
                unsafe { MapVirtualKeyExW(u32::from(scan), MAPVK_VSC_TO_VK_EX, Some(layout)) };
            let sample = |shift: bool, altgr: bool, caps: bool| {
                let mut state = [0u8; 256];
                state[VK_SHIFT.0 as usize] = if shift { 0x80 } else { 0 };
                state[VK_CONTROL.0 as usize] = if altgr { 0x80 } else { 0 };
                state[VK_MENU.0 as usize] = if altgr { 0x80 } else { 0 };
                state[VK_CAPITAL.0 as usize] = u8::from(caps);
                let mut buffer = [0u16; 16];
                // SAFETY: bounded arrays live through the call. Bit 2 prevents
                // sampling dead keys from changing the thread's keyboard state.
                let length = unsafe {
                    ToUnicodeEx(vkey, u32::from(scan), &state, &mut buffer, 4, Some(layout))
                };
                usize::try_from(length)
                    .ok()
                    .filter(|&n| n > 0 && n <= buffer.len())
                    .and_then(|n| String::from_utf16(&buffer[..n]).ok())
                    .filter(|text| !text.chars().any(char::is_control))
                    .unwrap_or_default()
            };
            gpui::KeyboardCharacterKey {
                usage,
                layers: [
                    sample(false, false, false),
                    sample(true, false, false),
                    sample(false, true, false),
                    sample(true, true, false),
                    sample(false, false, false),
                    sample(true, false, false),
                ],
                caps_lock_layers: Some([
                    sample(false, false, true),
                    sample(true, false, true),
                    sample(false, true, true),
                    sample(true, true, true),
                ]),
            }
        })
        .collect()
}

impl WindowsKeyboardMapper {
    pub(crate) fn new() -> Self {
        let mut key_to_vkey = HashMap::default();
        let mut vkey_to_key = HashMap::default();
        let mut vkey_to_shifted = HashMap::default();
        for vkey in CANDIDATE_VKEYS {
            if let Some(key) = get_key_from_vkey(*vkey) {
                key_to_vkey.insert(key.clone(), (vkey.0, false));
                vkey_to_key.insert(vkey.0, key);
            }
            let scan_code = unsafe { MapVirtualKeyW(vkey.0 as u32, MAPVK_VK_TO_VSC) };
            if scan_code == 0 {
                continue;
            }
            if let Some(shifted_key) = get_shifted_key(*vkey, scan_code) {
                key_to_vkey.insert(shifted_key.clone(), (vkey.0, true));
                vkey_to_shifted.insert(vkey.0, shifted_key);
            }
        }
        Self {
            key_to_vkey,
            vkey_to_key,
            vkey_to_shifted,
        }
    }

    fn get_vkey_from_key(&self, key: &str, use_key_equivalents: bool) -> Option<(u16, bool)> {
        if use_key_equivalents {
            get_vkey_from_key_with_us_layout(key)
        } else {
            self.key_to_vkey.get(key).cloned()
        }
    }
}

pub(crate) fn get_keystroke_key(
    vkey: VIRTUAL_KEY,
    scan_code: u32,
    modifiers: &mut Modifiers,
) -> Option<String> {
    if modifiers.shift && need_to_convert_to_shifted_key(vkey) {
        get_shifted_key(vkey, scan_code).inspect(|_| {
            modifiers.shift = false;
        })
    } else {
        get_key_from_vkey(vkey)
    }
}

fn get_key_from_vkey(vkey: VIRTUAL_KEY) -> Option<String> {
    let key_data = unsafe { MapVirtualKeyW(vkey.0 as u32, MAPVK_VK_TO_CHAR) };
    if key_data == 0 {
        return None;
    }

    // The high word contains dead key flag, the low word contains the character
    let key = char::from_u32(key_data & 0xFFFF)?;

    Some(key.to_ascii_lowercase().to_string())
}

#[inline]
fn need_to_convert_to_shifted_key(vkey: VIRTUAL_KEY) -> bool {
    matches!(
        vkey,
        VK_OEM_3
            | VK_OEM_MINUS
            | VK_OEM_PLUS
            | VK_OEM_4
            | VK_OEM_5
            | VK_OEM_6
            | VK_OEM_1
            | VK_OEM_7
            | VK_OEM_COMMA
            | VK_OEM_PERIOD
            | VK_OEM_2
            | VK_OEM_102
            | VK_OEM_8
            | VK_ABNT_C1
            | VK_0
            | VK_1
            | VK_2
            | VK_3
            | VK_4
            | VK_5
            | VK_6
            | VK_7
            | VK_8
            | VK_9
    )
}

fn get_shifted_key(vkey: VIRTUAL_KEY, scan_code: u32) -> Option<String> {
    generate_key_char(vkey, scan_code, false, true, false)
}

pub(crate) fn generate_key_char(
    vkey: VIRTUAL_KEY,
    scan_code: u32,
    control: bool,
    shift: bool,
    alt: bool,
) -> Option<String> {
    let mut state = [0; 256];
    if control {
        state[VK_CONTROL.0 as usize] = 0x80;
    }
    if shift {
        state[VK_SHIFT.0 as usize] = 0x80;
    }
    if alt {
        state[VK_MENU.0 as usize] = 0x80;
    }

    let mut buffer = [0; 8];
    let len = unsafe { ToUnicode(vkey.0 as u32, scan_code, Some(&state), &mut buffer, 0x5) };

    match len {
        len if len > 0 => String::from_utf16(&buffer[..len as usize])
            .ok()
            .filter(|candidate| {
                !candidate.is_empty() && !candidate.chars().next().unwrap().is_control()
            }),
        len if len < 0 => String::from_utf16(&buffer[..(-len as usize)]).ok(),
        _ => None,
    }
}

fn get_vkey_from_key_with_us_layout(key: &str) -> Option<(u16, bool)> {
    match key {
        // ` => VK_OEM_3
        "`" => Some((VK_OEM_3.0, false)),
        "~" => Some((VK_OEM_3.0, true)),
        "1" => Some((VK_1.0, false)),
        "!" => Some((VK_1.0, true)),
        "2" => Some((VK_2.0, false)),
        "@" => Some((VK_2.0, true)),
        "3" => Some((VK_3.0, false)),
        "#" => Some((VK_3.0, true)),
        "4" => Some((VK_4.0, false)),
        "$" => Some((VK_4.0, true)),
        "5" => Some((VK_5.0, false)),
        "%" => Some((VK_5.0, true)),
        "6" => Some((VK_6.0, false)),
        "^" => Some((VK_6.0, true)),
        "7" => Some((VK_7.0, false)),
        "&" => Some((VK_7.0, true)),
        "8" => Some((VK_8.0, false)),
        "*" => Some((VK_8.0, true)),
        "9" => Some((VK_9.0, false)),
        "(" => Some((VK_9.0, true)),
        "0" => Some((VK_0.0, false)),
        ")" => Some((VK_0.0, true)),
        "-" => Some((VK_OEM_MINUS.0, false)),
        "_" => Some((VK_OEM_MINUS.0, true)),
        "=" => Some((VK_OEM_PLUS.0, false)),
        "+" => Some((VK_OEM_PLUS.0, true)),
        "[" => Some((VK_OEM_4.0, false)),
        "{" => Some((VK_OEM_4.0, true)),
        "]" => Some((VK_OEM_6.0, false)),
        "}" => Some((VK_OEM_6.0, true)),
        "\\" => Some((VK_OEM_5.0, false)),
        "|" => Some((VK_OEM_5.0, true)),
        ";" => Some((VK_OEM_1.0, false)),
        ":" => Some((VK_OEM_1.0, true)),
        "'" => Some((VK_OEM_7.0, false)),
        "\"" => Some((VK_OEM_7.0, true)),
        "," => Some((VK_OEM_COMMA.0, false)),
        "<" => Some((VK_OEM_COMMA.0, true)),
        "." => Some((VK_OEM_PERIOD.0, false)),
        ">" => Some((VK_OEM_PERIOD.0, true)),
        "/" => Some((VK_OEM_2.0, false)),
        "?" => Some((VK_OEM_2.0, true)),
        _ => None,
    }
}

const CANDIDATE_VKEYS: &[VIRTUAL_KEY] = &[
    VK_OEM_3,
    VK_OEM_MINUS,
    VK_OEM_PLUS,
    VK_OEM_4,
    VK_OEM_5,
    VK_OEM_6,
    VK_OEM_1,
    VK_OEM_7,
    VK_OEM_COMMA,
    VK_OEM_PERIOD,
    VK_OEM_2,
    VK_OEM_102,
    VK_OEM_8,
    VK_ABNT_C1,
    VK_0,
    VK_1,
    VK_2,
    VK_3,
    VK_4,
    VK_5,
    VK_6,
    VK_7,
    VK_8,
    VK_9,
];

#[cfg(test)]
mod tests {
    use crate::WindowsKeyboardMapper;
    use gpui::{Keystroke, Modifiers, PlatformKeyboardMapper};

    #[test]
    fn printable_positions_are_unique_and_exclude_extended_keys() {
        let mut scans = std::collections::HashSet::new();
        let mut usages = std::collections::HashSet::new();
        for &(scan, usage) in super::CHARACTER_POSITIONS {
            assert!(scans.insert(scan));
            assert!(usages.insert(usage));
            assert_eq!(super::character_usage(scan), Some(usage));
            assert_eq!(super::character_usage(scan | 0x100), None);
        }
        assert_eq!(super::character_usage(0x1e), Some(0x04));
        assert_eq!(super::character_usage(0x39), Some(0x2c));
        let keys = super::character_keys();
        assert_eq!(keys.len(), scans.len());
        assert!(keys.iter().all(|key| key.caps_lock_layers.is_some()));
    }

    #[test]
    fn test_keyboard_mapper() {
        let mapper = WindowsKeyboardMapper::new();

        // Normal case
        let keystroke = Keystroke {
            modifiers: Modifiers::control(),
            key: "a".to_string(),
            key_char: None,
        };
        let mapped = mapper.map_key_equivalent(keystroke.clone(), true);
        assert_eq!(*mapped.inner(), keystroke);
        assert_eq!(mapped.key(), "a");
        assert_eq!(*mapped.modifiers(), Modifiers::control());

        // Shifted case, ctrl-$
        let keystroke = Keystroke {
            modifiers: Modifiers::control(),
            key: "$".to_string(),
            key_char: None,
        };
        let mapped = mapper.map_key_equivalent(keystroke.clone(), true);
        assert_eq!(*mapped.inner(), keystroke);
        assert_eq!(mapped.key(), "4");
        assert_eq!(*mapped.modifiers(), Modifiers::control_shift());

        // Shifted case, but shift is true
        let keystroke = Keystroke {
            modifiers: Modifiers::control_shift(),
            key: "$".to_string(),
            key_char: None,
        };
        let mapped = mapper.map_key_equivalent(keystroke, true);
        assert_eq!(mapped.inner().modifiers, Modifiers::control());
        assert_eq!(mapped.key(), "4");
        assert_eq!(*mapped.modifiers(), Modifiers::control_shift());

        // Windows style
        let keystroke = Keystroke {
            modifiers: Modifiers::control_shift(),
            key: "4".to_string(),
            key_char: None,
        };
        let mapped = mapper.map_key_equivalent(keystroke, true);
        assert_eq!(mapped.inner().modifiers, Modifiers::control());
        assert_eq!(mapped.inner().key, "$");
        assert_eq!(mapped.key(), "4");
        assert_eq!(*mapped.modifiers(), Modifiers::control_shift());
    }
}
