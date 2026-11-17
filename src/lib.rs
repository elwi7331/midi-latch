use lv2::lv2_atom::sequence::{SequenceWriter, TimeStampUnit};
use lv2::prelude::*;
use wmidi::{Channel, MidiMessage, Note, U7};

/// Parsed plugin controls entered by the user in the plugin UI.
#[derive(Copy, Clone, Debug)]
struct PluginControls {
    cc_channel: Channel,
    cc_number: U7,
    notes_channel: Channel,
}

impl PluginControls {
    fn new(cc_channel: Channel, cc_number: U7, notes_channel: Channel) -> Self {
        Self {
            cc_channel,
            cc_number,
            notes_channel,
        }
    }
}

impl From<&Ports> for PluginControls {
    fn from(ports: &Ports) -> Self {
        let cc_channel = ports.parse_cc_channel();
        let cc_number = ports.parse_cc_number();
        let notes_channel = ports.parse_notes_channel();

        Self::new(cc_channel, cc_number, notes_channel)
    }
}

/// Exposes UI controls and midi IO to the host
#[derive(PortCollection)]
pub struct Ports {
    input: InputPort<AtomPort>,
    output: OutputPort<AtomPort>,
    cc_channel: InputPort<Control>,
    cc_number: InputPort<Control>,
    notes_channel: InputPort<Control>,
}

impl Ports {
    fn parse_notes_channel(&self) -> Channel {
        let channel = self.notes_channel.round().clamp(1.0, 16.0) as u8;
        // safe to unwrap because value is clamped to 1-16
        Channel::from_index(channel - 1).unwrap()
    }

    fn parse_cc_channel(&self) -> Channel {
        let channel = self.cc_channel.round().clamp(1.0, 16.0) as u8;
        // safe to unwrap because value is clamped to 1-16
        Channel::from_index(channel - 1).unwrap()
    }

    fn parse_cc_number(&self) -> U7 {
        let cc_number = self.cc_number.round().clamp(0.0, 127.0) as u8;
        // safe to unwrap because value is clamped to 0-127
        U7::try_from(cc_number).unwrap()
    }
}

#[derive(FeatureCollection)]
pub struct Features<'a> {
    map: LV2Map<'a>,
}

#[derive(URIDCollection)]
pub struct URIDs {
    atom: AtomURIDCollection,
    unit: UnitURIDCollection,
    midi: MidiURIDCollection,
}

/// Represents currently held notes/latched notes as a bitfield.
#[derive(Copy, Clone, Debug)]
struct HeldNotes {
    // notes 0-63
    lower_bits: u64,
    // notes 64-127
    upper_bits: u64,
}

impl HeldNotes {
    fn new() -> Self {
        Self {
            lower_bits: 0,
            upper_bits: 0,
        }
    }

    fn set(&mut self, note: Note) {
        let note = u8::from(note);
        if note < 64 {
            self.lower_bits |= 1 << note;
        } else {
            self.upper_bits |= 1 << (note - 64);
        }
    }

    fn clear(&mut self, note: Note) {
        let note = u8::from(note);
        if note < 64 {
            self.lower_bits &= !(1 << note);
        } else {
            self.upper_bits &= !(1 << (note - 64));
        }
    }

    fn get(&self, note: Note) -> bool {
        let note = u8::from(note);
        if note < 64 {
            self.lower_bits & (1 << note) != 0
        } else {
            self.upper_bits & (1 << (note - 64)) != 0
        }
    }
}

/// Iterator over the currently held notes in a `HeldNotes` bitfield.
struct HeldNotesIter<'a> {
    held: &'a HeldNotes,
    next_index: u8,
}

impl<'a> From<&'a HeldNotes> for HeldNotesIter<'a> {
    fn from(held: &'a HeldNotes) -> Self {
        Self {
            held,
            next_index: 0,
        }
    }
}

impl Iterator for HeldNotesIter<'_> {
    type Item = Note;

    fn next(&mut self) -> Option<Self::Item> {
        while self.next_index < 128 {
            // safe to unwrap because next_index is always in the range 0-127
            let note = Note::try_from(self.next_index).unwrap();
            self.next_index += 1;

            if self.held.get(note) {
                return Some(note);
            }
        }
        None
    }
}

#[uri("urn:midi-latch")]
pub struct MidiLatch {
    urids: URIDs,
    latch: Option<HeldNotes>,
    held_notes: HeldNotes,
}

impl MidiLatch {
    /// Main event handling function. Processes incoming MIDI events and applies the latching logic.
    ///
    /// Idea:
    ///     * Keep track of the currently held notes in `self.held_notes`:
    ///         - When a NoteOn event is received, we add the note to `self.held_notes`.
    ///         - When a NoteOff event is received, we remove the note from `self.held_notes`.
    ///     * When the latch pedal (CC) is pressed:
    ///         - store the currently held notes in `self.latch`.
    ///         - delay sending NoteOff events for the latched notes until the latch pedal is released.
    ///     * When the latch pedal (CC) is released:
    ///         - send NoteOff events (except for currently held notes)
    ///         - clear the latch.
    ///         
    fn handle_event(
        &mut self,
        timestamp: TimeStamp,
        event: UnidentifiedAtom<'_>,
        controls: &PluginControls,
        output_sequence: &mut SequenceWriter,
    ) {
        match event.read(self.urids.midi.wmidi, ()) {
            // NoteOff or NoteOn with velocity 0, on the plugin's selected notes-channel
            Some(
                MidiMessage::NoteOff(channel, note, _)
                | MidiMessage::NoteOn(channel, note, wmidi::U7::MIN),
            ) if channel == controls.notes_channel => {
                self.held_notes.clear(note);

                // If the note is not latched, forward the NoteOff event
                if self.latch.is_none_or(|latch| !latch.get(note)) {
                    output_sequence.forward(timestamp, event).unwrap();
                }
            }

            // NoteOn on the plugin's selected notes-channel
            Some(MidiMessage::NoteOn(channel, note, _)) if channel == controls.notes_channel => {
                self.held_notes.set(note);
                output_sequence.forward(timestamp, event).unwrap();
            }

            // Control Change on the plugin's selected CC channel and number
            Some(MidiMessage::ControlChange(channel, cc, value))
                if channel == controls.cc_channel && cc == controls.cc_number =>
            {
                // Latch activation
                if value > U7::MIN {
                    self.latch = Some(self.held_notes);

                // Latch deactivation:
                // send Note Off for all latched notes that are not currently held, then clear the latch
                } else if let Some(latch) = self.latch {
                    for note in
                        HeldNotesIter::from(&latch).filter(|note| !self.held_notes.get(*note))
                    {
                        let note_off_message = MidiMessage::NoteOff(
                            controls.notes_channel,
                            note,
                            // default velocity for Note Off is used, to avoid storing the velocity of each delayed Note Off event
                            wmidi::Velocity::try_from(64).unwrap(),
                        );
                        output_sequence
                            .init(timestamp, self.urids.midi.wmidi, note_off_message)
                            .unwrap();
                    }
                    // Clear the latch after sending all Note Off messages
                    self.latch = None;
                }
            }
            _ => {
                // Forward all other events
                output_sequence.forward(timestamp, event).unwrap();
            }
        }
    }
}

impl Plugin for MidiLatch {
    type Ports = Ports;
    type InitFeatures = Features<'static>;
    type AudioFeatures = ();

    fn new(_plugin_info: &PluginInfo, features: &mut Features<'static>) -> Option<Self> {
        Some(Self {
            urids: features.map.populate_collection()?,
            held_notes: HeldNotes::new(),
            latch: None,
        })
    }

    fn run(&mut self, ports: &mut Ports, _: &mut (), _: u32) {
        let controls = PluginControls::from(&(*ports));

        // begin boiler
        let input_sequence = ports
            .input
            .read(self.urids.atom.sequence, self.urids.unit.beat)
            .unwrap();

        let output_unit = match input_sequence.unit() {
            TimeStampUnit::Frames => TimeStampURID::Frames(self.urids.unit.frame),
            TimeStampUnit::BeatsPerMinute => TimeStampURID::BeatsPerMinute(self.urids.unit.beat),
        };
        let mut output_sequence = ports
            .output
            .init(self.urids.atom.sequence, output_unit)
            .unwrap();
        // end boiler

        // Process events in the input sequence
        for (timestamp, event) in input_sequence {
            self.handle_event(timestamp, event, &controls, &mut output_sequence);
        }
    }
}

lv2_descriptors!(MidiLatch);
