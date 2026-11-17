use lv2::lv2_atom::sequence::TimeStampUnit;
use lv2::prelude::*;
use wmidi::{Channel, MidiMessage};

#[derive(PortCollection)]
pub struct Ports {
    input: InputPort<AtomPort>,
    output: OutputPort<AtomPort>,
    cc_midi_channel: InputPort<Control>,
    cc_number: InputPort<Control>,
    notes_midi_channel: InputPort<Control>,
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

#[derive(Copy, Clone, Debug)]
struct MidiChannel {
    // notes 0-63
    lower_bits: u64,
    // notes 64-127
    upper_bits: u64,
}

impl MidiChannel {
    fn new() -> Self {
        Self {
            lower_bits: 0,
            upper_bits: 0,
        }
    }

    // todo: use u7
    fn set(&mut self, note: u8) {
        if note < 64 {
            self.lower_bits |= 1 << note;
        } else {
            self.upper_bits |= 1 << (note - 64);
        }
    }

    fn clear(&mut self, note: u8) {
        if note < 64 {
            self.lower_bits &= !(1 << note);
        } else {
            self.upper_bits &= !(1 << (note - 64));
        }
    }

    fn get(&self, note: u8) -> bool {
        if note < 64 {
            self.lower_bits & (1 << note) != 0
        } else {
            self.upper_bits & (1 << (note - 64)) != 0
        }
    }
}

#[derive(Copy, Clone, Debug)]
struct HeldNotes {
    channel: MidiChannel,
}

impl HeldNotes {
    fn new() -> Self {
        Self {
            channel: MidiChannel::new(),
        }
    }

    fn set(&mut self, note: u8) {
        self.channel.set(note);
    }

    fn clear(&mut self, note: u8) {
        self.channel.clear(note);
    }

    fn get(&self, note: u8) -> bool {
        self.channel.get(note)
    }

    fn notes(&self) -> NoteIter<'_> {
        NoteIter::from(self).into_iter()
    }
}

struct NoteIter<'a> {
    held: &'a HeldNotes,
    next_index: u8,
}

impl<'a> From<&'a HeldNotes> for NoteIter<'a> {
    fn from(held: &'a HeldNotes) -> Self {
        Self {
            held,
            next_index: 0,
        }
    }
}

impl Iterator for NoteIter<'_> {
    type Item = u8;

    fn next(&mut self) -> Option<Self::Item> {
        while self.next_index < 128 {
            let note = self.next_index;
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

        let note_channel =
            Channel::from_index(ports.notes_midi_channel.round().clamp(1.0, 16.0) as u8 - 1)
                .unwrap();
        let cc_channel =
            Channel::from_index(ports.cc_midi_channel.round().clamp(1.0, 16.0) as u8 - 1).unwrap();
        let cc_number =
            wmidi::U7::try_from(ports.cc_number.round().clamp(0.0, 127.0) as u8).unwrap();

        for (timestamp, event) in input_sequence {
            match event.read(self.urids.midi.wmidi, ()) {
                Some(
                    MidiMessage::NoteOff(channel, note, _)
                    | MidiMessage::NoteOn(channel, note, wmidi::U7::MIN),
                ) if channel == note_channel => {
                    self.held_notes.clear(u8::from(note));

                    if let Some(latch) = self.latch
                        && latch.get(u8::from(note))
                    {
                        ()
                    } else {
                        output_sequence.forward(timestamp, event).unwrap();
                    }
                }

                Some(MidiMessage::NoteOn(channel, note, _)) if channel == note_channel => {
                    self.held_notes.set(u8::from(note));
                    output_sequence.forward(timestamp, event).unwrap();
                }

                Some(MidiMessage::ControlChange(channel, cc, value))
                    if channel == cc_channel && cc == cc_number =>
                {
                    // value > 0 <=> pedal pressed
                    if u8::from(value) > 0 {
                        // latch the currently held notes
                        self.latch = Some(self.held_notes);

                    // value == 0 <=> pedal released
                    } else if let Some(latch) = self.latch {
                        // loop through all latched notes except those that currently held
                        for note in latch.notes().filter(|note| !self.held_notes.get(*note)) {
                            // all latched notes that are not held are sent as note off
                            let note_off_message = MidiMessage::NoteOff(
                                channel,
                                wmidi::Note::try_from(note).unwrap(),
                                wmidi::Velocity::try_from(64).unwrap(),
                            );
                            output_sequence
                                .init(timestamp, self.urids.midi.wmidi, note_off_message)
                                .unwrap();
                        }
                        // clear the latch after sending all note off messages
                        self.latch = None;
                    }
                }
                // forward all other events
                Some(..) | None => {
                    output_sequence.forward(timestamp, event).unwrap();
                }
            }
        }
    }
}

lv2_descriptors!(MidiLatch);
