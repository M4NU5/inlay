//! Which microphone records, as on the Mac: saved priority lists of inputs,
//! and the choice between the active list, the Windows default input and one
//! fixed microphone. Inputs are known by their Windows endpoint ID, which
//! stays the same when a microphone is unplugged and plugged in again; the
//! name is kept to show while it is away.

use serde::{Deserialize, Serialize};

/// An audio input, by its Windows endpoint ID.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Input {
    pub uid: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub priority: Vec<Input>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase")]
pub enum Selection {
    /// The first connected microphone in the active list.
    Automatic,
    SystemDefault,
    Fixed {
        device: Input,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Microphones {
    pub profiles: Vec<Profile>,
    #[serde(rename = "activeProfileID")]
    pub active_profile_id: String,
    pub selection: Selection,
}

impl Default for Microphones {
    fn default() -> Microphones {
        Microphones {
            profiles: vec![default_profile()],
            active_profile_id: "default".into(),
            selection: Selection::Automatic,
        }
    }
}

fn default_profile() -> Profile {
    Profile {
        id: "default".into(),
        name: "Default".into(),
        priority: Vec::new(),
    }
}

/// Why the next take uses the microphone it does.
#[derive(Clone, PartialEq, Eq)]
pub enum Reason {
    Priority,
    Fixed,
    SystemDefault,
    /// The fixed microphone is away, so the default input stands in.
    Fallback(Input),
    Unavailable,
}

pub struct Resolution {
    pub device: Option<Input>,
    pub reason: Reason,
}

impl Microphones {
    pub fn active(&self) -> &Profile {
        self.profiles
            .iter()
            .find(|profile| profile.id == self.active_profile_id)
            .or(self.profiles.first())
            .expect("normalized preferences keep a list")
    }

    /// Mends saved preferences, as the Mac does after every edit: at least
    /// one list, unique ids and inputs, names that aren't blank, and an
    /// active list that exists.
    pub fn normalized(mut self) -> Microphones {
        let mut ids = Vec::new();
        self.profiles.retain_mut(|profile| {
            if profile.id.trim().is_empty() || ids.contains(&profile.id) {
                return false;
            }
            ids.push(profile.id.clone());
            let name = profile.name.trim();
            profile.name = if name.is_empty() { "Default" } else { name }.into();
            unique(&mut profile.priority);
            true
        });
        if self.profiles.is_empty() {
            self.profiles.push(default_profile());
        }
        if !ids.contains(&self.active_profile_id) {
            self.active_profile_id = self.profiles[0].id.clone();
        }
        if let Selection::Fixed { device } = &self.selection
            && device.uid.trim().is_empty()
        {
            self.selection = Selection::Automatic;
        }
        self
    }

    /// The microphones a take tries, in order, before the Windows default.
    pub fn wanted(&self) -> Vec<String> {
        match &self.selection {
            Selection::Automatic => self
                .active()
                .priority
                .iter()
                .map(|input| input.uid.clone())
                .collect(),
            Selection::SystemDefault => Vec::new(),
            Selection::Fixed { device } => vec![device.uid.clone()],
        }
    }

    /// Which microphone the next take uses, and why. Matches the take's own
    /// choice: the first of `wanted` that is connected, then the default.
    pub fn resolve(&self, inputs: &[Input], default: Option<&Input>) -> Resolution {
        let wanted = self.wanted();
        if let Some(device) = wanted
            .iter()
            .find_map(|uid| inputs.iter().find(|input| input.uid == *uid))
        {
            let reason = match self.selection {
                Selection::Fixed { .. } => Reason::Fixed,
                _ => Reason::Priority,
            };
            return Resolution {
                device: Some(device.clone()),
                reason,
            };
        }
        let reason = match (&self.selection, default) {
            (_, None) => Reason::Unavailable,
            (Selection::Fixed { device }, _) => Reason::Fallback(device.clone()),
            _ => Reason::SystemDefault,
        };
        Resolution {
            device: default.cloned(),
            reason,
        }
    }

    /// Takes the names connected inputs have now, without forgetting the
    /// order of the ones that are away.
    pub fn rename_connected(&mut self, inputs: &[Input]) {
        let current = |input: &mut Input| {
            if let Some(connected) = inputs.iter().find(|each| each.uid == input.uid) {
                input.name.clone_from(&connected.name);
            }
        };
        for profile in &mut self.profiles {
            profile.priority.iter_mut().for_each(current);
        }
        if let Selection::Fixed { device } = &mut self.selection {
            current(device);
        }
    }

    pub fn add_profile(&mut self, name: &str) -> Result<(), String> {
        let name = self.valid_name(name, None)?;
        let id = uuid::Uuid::new_v4().to_string().to_uppercase();
        self.profiles.push(Profile {
            id: id.clone(),
            name,
            priority: Vec::new(),
        });
        self.active_profile_id = id;
        Ok(())
    }

    pub fn rename_profile(&mut self, id: &str, name: &str) -> Result<(), String> {
        let name = self.valid_name(name, Some(id))?;
        let profile = self
            .profiles
            .iter_mut()
            .find(|profile| profile.id == id)
            .ok_or("That priority list is no longer available.")?;
        profile.name = name;
        Ok(())
    }

    pub fn remove_profile(&mut self, id: &str) {
        if self.profiles.len() > 1 {
            self.profiles.retain(|profile| profile.id != id);
        }
    }

    fn valid_name(&self, name: &str, excluding: Option<&str>) -> Result<String, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("Give the priority list a name.".into());
        }
        let folded = name.to_lowercase();
        if self.profiles.iter().any(|profile| {
            Some(profile.id.as_str()) != excluding && profile.name.to_lowercase() == folded
        }) {
            return Err("A priority list already has that name.".into());
        }
        Ok(name.into())
    }

    /// The active list's inputs, to edit.
    pub fn priority_mut(&mut self) -> &mut Vec<Input> {
        let id = self.active().id.clone();
        let profile = self
            .profiles
            .iter_mut()
            .find(|profile| profile.id == id)
            .expect("the active list exists");
        &mut profile.priority
    }

    pub fn add_to_priority(&mut self, input: Input) {
        let priority = self.priority_mut();
        if !priority.iter().any(|each| each.uid == input.uid) {
            priority.push(input);
        }
    }

    pub fn remove_from_priority(&mut self, uid: &str) {
        self.priority_mut().retain(|input| input.uid != uid);
    }

    /// Moves an input one place up (`-1`) or down (`1`).
    pub fn move_priority(&mut self, uid: &str, offset: isize) {
        let priority = self.priority_mut();
        if let Some(index) = priority.iter().position(|input| input.uid == uid)
            && let Some(target) = index.checked_add_signed(offset)
            && target < priority.len()
        {
            priority.swap(index, target);
        }
    }
}

fn unique(inputs: &mut Vec<Input>) {
    let mut seen = Vec::new();
    inputs.retain_mut(|input| {
        if input.uid.trim().is_empty() || seen.contains(&input.uid) {
            return false;
        }
        seen.push(input.uid.clone());
        let name = input.name.trim();
        input.name = if name.is_empty() { "Microphone" } else { name }.into();
        true
    });
}
