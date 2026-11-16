use lv2::lv2_atom::sequence::TimeStampUnit;
use lv2::prelude::*;
use wmidi::{MidiMessage};

#[derive(PortCollection)]
pub struct Ports {
    input: InputPort<AtomPort>,
    output: OutputPort<AtomPort>,
    cc_number: InputPort<Control>,
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
struct PendingNoteOffs(u64, u64);

impl PendingNoteOffs {
    fn new() -> Self {
        Self(0, 0)
    }
    
    fn set(&mut self, note: u8) -> Result<(), &'static str> {
        if note > 127 {
            return Err("Note value must be between 0 and 127");
        }
        
       if note < 64 {
            self.0 |= 1 << note;
        } else {
            self.1 |= 1 << (note - 64);
        }
        
        Ok(())
    }
    
    fn clear(&mut self, note: u8) -> Result<(), &'static str> {
        if note > 127{
            return Err("Note value must be between 0 and 127");
        }
        
        if note < 64{
            self.0 &= !(1 << note);
        } else {
            self.1 &= !(1 << (note - 64));
        }
        
        Ok(())
    }
    
    fn notes(&self) -> NoteIter<'_> {
        NoteIter::from(self).into_iter()
    }
}

struct NoteIter<'a> {
    pending: &'a PendingNoteOffs
}

impl<'a> From<&'a PendingNoteOffs> for NoteIter<'a> {
    fn from(pending: &'a PendingNoteOffs) -> Self {
        Self { pending }
    }
}

impl Iterator for NoteIter<'_> {
    type Item = u8;

    fn next(&mut self) -> Option<Self::Item> {
        for i in 0..64 {
            if (self.pending.0 & (1 << i)) != 0 {
                return Some(i);
            }
        }
        for i in 0..64 {
            if (self.pending.1 & (1 << i)) != 0 {
                return Some(i + 64);
            }
        }
        None
    }
}

#[uri("urn:midi-latch")]
pub struct MidiLatch {
    urids: URIDs,
    latch_active: bool,
    pending_note_offs: PendingNoteOffs,
}

enum EventAction {
    ControlValue(u8),
    DelayNoteOff(MidiMessage<'static>),
    Forward,
}

impl Plugin for MidiLatch {
    type Ports = Ports;
    type InitFeatures = Features<'static>;
    type AudioFeatures = ();

    fn new(_plugin_info: &PluginInfo, features: &mut Features<'static>) -> Option<Self> {
        Some(Self {
            urids: features.map.populate_collection()?,
            latch_active: false,
            pending_note_offs: PendingNoteOffs::new(),
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

        let cc_number = (*ports.cc_number).round().clamp(0.0, 127.0) as u8;

        for (timestamp, event) in input_sequence {
            match event.read(self.urids.midi.wmidi, ()) {
                Some(MidiMessage::NoteOn(_, note, velocity)) => {
                    self.pending_note_offs.clear(u8::from(note)).unwrap();
                    output_sequence.forward(timestamp, event).unwrap();
                },
                // NoteOff velocity is ignored in the pending note offs
                Some(MidiMessage::NoteOff(_, note, _)) => {
                    if self.latch_active {
                        self.pending_note_offs.set(u8::from(note)).unwrap();
                    } else {
                        output_sequence.forward(timestamp, event).unwrap();
                    }
                },
                // TODO handle channel better. Now hardcoded same as cc
                Some(MidiMessage::ControlChange(channel, cc, value)) if u8::from(value) == cc_number => {
                    for note_off in self.pending_note_offs.notes() {
                        let note_off_message = MidiMessage::NoteOff(
                            channel,
                            wmidi::Note::try_from(note_off).unwrap(),
                            wmidi::Velocity::try_from(64).unwrap(),
                        );
                        output_sequence
                            .init(timestamp, self.urids.midi.wmidi, note_off_message)
                            .unwrap();
                    }
                } 
                Some(..) | None => {
                    output_sequence.forward(timestamp, event).unwrap();
                },
            }
        }
    }
}

lv2_descriptors!(MidiLatch);
