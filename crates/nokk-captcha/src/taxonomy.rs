//! Prompt taxonomy: canonical labels, aliases, thresholds.
//!
//! Top-50 canonical labels (vehicles, animals, street scenes). Unknown
//! prompts fall back to a strict default threshold instead of a guess —
//! fail-closed. Multi-word labels use `snake_case` (`traffic_light`).

/// Canonical top-50 labels: 10 vehicles + 20 animals + 20 street/scene.
pub const LABELS: &[&str] = &[
    // Vehicles (10).
    "truck",
    "car",
    "bus",
    "motorcycle",
    "bicycle",
    "airplane",
    "helicopter",
    "boat",
    "train",
    "tram",
    // Animals (20).
    "dog",
    "cat",
    "bird",
    "horse",
    "cow",
    "sheep",
    "lion",
    "tiger",
    "bear",
    "elephant",
    "zebra",
    "giraffe",
    "monkey",
    "rabbit",
    "deer",
    "frog",
    "fish",
    "shark",
    "whale",
    "snake",
    // Street / scene (20).
    "traffic_light",
    "bicycle_rack",
    "bridge",
    "motorbus_stop",
    "pedestrian",
    "tree",
    "building",
    "mountain",
    "water",
    "road",
    "stop_sign",
    "crosswalk",
    "fire_hydrant",
    "street_light",
    "tower",
    "statue",
    "fountain",
    "tunnel",
    "traffic_cone",
    "taxi",
];

/// (alias, canonical) pairs, all lowercase, spaces (not underscores).
/// Plurals and US/UK variants included so `normalize_prompt` lands on the
/// canonical key without guessing.
const ALIASES: &[(&str, &str)] = &[
    // --- vehicles ---
    ("lorry", "truck"),
    ("lorries", "truck"),
    ("lorrys", "truck"),
    ("trucks", "truck"),
    ("pickup", "truck"),
    ("pickup truck", "truck"),
    ("fire truck", "truck"),
    ("semi", "truck"),
    ("semi truck", "truck"),
    ("automobile", "car"),
    ("automobiles", "car"),
    ("cars", "car"),
    ("sedan", "car"),
    ("sedans", "car"),
    ("suv", "car"),
    ("buses", "bus"),
    ("busess", "bus"),
    ("coach", "bus"),
    ("coaches", "bus"),
    ("motorbus", "bus"),
    ("motorbuses", "bus"),
    ("motorbike", "motorcycle"),
    ("motor bike", "motorcycle"),
    ("motorbikes", "motorcycle"),
    ("motorcycles", "motorcycle"),
    ("motor cycle", "motorcycle"),
    ("bike motor", "motorcycle"),
    ("bike", "bicycle"),
    ("bikes", "bicycle"),
    ("cycle", "bicycle"),
    ("cycles", "bicycle"),
    ("bicycles", "bicycle"),
    ("pushbike", "bicycle"),
    ("push bike", "bicycle"),
    ("aeroplane", "airplane"),
    ("aeroplanes", "airplane"),
    ("airplanes", "airplane"),
    ("plane", "airplane"),
    ("planes", "airplane"),
    ("aircraft", "airplane"),
    ("jet", "airplane"),
    ("jets", "airplane"),
    ("chopper", "helicopter"),
    ("choppers", "helicopter"),
    ("helicopters", "helicopter"),
    ("heli", "helicopter"),
    ("ship", "boat"),
    ("ships", "boat"),
    ("ferry", "boat"),
    ("ferries", "boat"),
    ("boats", "boat"),
    ("yacht", "boat"),
    ("yachts", "boat"),
    ("sailboat", "boat"),
    ("subway", "train"),
    ("subways", "train"),
    ("trains", "train"),
    ("locomotive", "train"),
    ("locomotives", "train"),
    ("rail", "train"),
    ("streetcar", "tram"),
    ("streetcars", "tram"),
    ("trolley", "tram"),
    ("trolleys", "tram"),
    ("trams", "tram"),
    ("light rail", "tram"),
    // taxi (vehicle, late addition).
    ("taxis", "taxi"),
    ("cab", "taxi"),
    ("cabs", "taxi"),
    ("taxicab", "taxi"),
    // --- animals ---
    ("puppy", "dog"),
    ("puppies", "dog"),
    ("dogs", "dog"),
    ("hound", "dog"),
    ("hounds", "dog"),
    ("kitten", "cat"),
    ("kittens", "cat"),
    ("cats", "cat"),
    ("kitty", "cat"),
    ("birds", "bird"),
    ("seagull", "bird"),
    ("seagulls", "bird"),
    ("dove", "bird"),
    ("doves", "bird"),
    ("pigeon", "bird"),
    ("pigeons", "bird"),
    ("pony", "horse"),
    ("ponies", "horse"),
    ("horses", "horse"),
    ("stallion", "horse"),
    ("cows", "cow"),
    ("calf", "cow"),
    ("calves", "cow"),
    ("bull", "cow"),
    ("sheep", "sheep"),
    ("lamb", "sheep"),
    ("lambs", "sheep"),
    ("cub", "bear"),
    ("cubs", "bear"),
    ("bears", "bear"),
    ("grizzly", "bear"),
    ("lions", "lion"),
    ("lioness", "lion"),
    ("tigers", "tiger"),
    ("elephants", "elephant"),
    ("zebras", "zebra"),
    ("giraffes", "giraffe"),
    ("monkeys", "monkey"),
    ("ape", "monkey"),
    ("apes", "monkey"),
    ("rabbits", "rabbit"),
    ("bunny", "rabbit"),
    ("bunnies", "rabbit"),
    ("hares", "rabbit"),
    ("deers", "deer"),
    ("stag", "deer"),
    ("frogs", "frog"),
    ("toad", "frog"),
    ("toads", "frog"),
    ("fishes", "fish"),
    ("goldfish", "fish"),
    ("sharks", "shark"),
    ("dolphin", "whale"),
    ("dolphins", "whale"),
    ("whales", "whale"),
    ("crocodile", "snake"),
    ("crocodiles", "snake"),
    ("snakes", "snake"),
    ("serpent", "snake"),
    ("python", "snake"),
    // --- street / scene ---
    ("traffic lights", "traffic_light"),
    ("stoplight", "traffic_light"),
    ("stoplights", "traffic_light"),
    ("signal", "traffic_light"),
    ("signals", "traffic_light"),
    ("traffic signal", "traffic_light"),
    ("bicycle racks", "bicycle_rack"),
    ("bike rack", "bicycle_rack"),
    ("bike racks", "bicycle_rack"),
    ("bridges", "bridge"),
    ("overpass", "bridge"),
    ("bus stop", "motorbus_stop"),
    ("bus stops", "motorbus_stop"),
    ("motorbus stops", "motorbus_stop"),
    ("person", "pedestrian"),
    ("people", "pedestrian"),
    ("persons", "pedestrian"),
    ("pedestrians", "pedestrian"),
    ("walker", "pedestrian"),
    ("walkers", "pedestrian"),
    ("human", "pedestrian"),
    ("humans", "pedestrian"),
    ("man walking", "pedestrian"),
    ("trees", "tree"),
    ("palm", "tree"),
    ("palms", "tree"),
    ("palm tree", "tree"),
    ("oak", "tree"),
    ("buildings", "building"),
    ("house", "building"),
    ("houses", "building"),
    ("skyscraper", "building"),
    ("skyscrapers", "building"),
    ("apartment", "building"),
    ("mountains", "mountain"),
    ("hill", "mountain"),
    ("hills", "mountain"),
    ("peak", "mountain"),
    ("waters", "water"),
    ("river", "water"),
    ("lake", "water"),
    ("ocean", "water"),
    ("sea", "water"),
    ("roads", "road"),
    ("street", "road"),
    ("streets", "road"),
    ("highway", "road"),
    ("stop signs", "stop_sign"),
    ("stop sign", "stop_sign"),
    ("crosswalks", "crosswalk"),
    ("zebra crossing", "crosswalk"),
    ("pedestrian crossing", "crosswalk"),
    ("fire hydrants", "fire_hydrant"),
    ("hydrant", "fire_hydrant"),
    ("street lights", "street_light"),
    ("streetlight", "street_light"),
    ("streetlights", "street_light"),
    ("street lamp", "street_light"),
    ("lamp post", "street_light"),
    ("towers", "tower"),
    ("clock tower", "tower"),
    ("statues", "statue"),
    ("sculpture", "statue"),
    ("fountains", "fountain"),
    ("tunnels", "tunnel"),
    ("underpass", "tunnel"),
    ("traffic cones", "traffic_cone"),
    ("cone", "traffic_cone"),
    ("cones", "traffic_cone"),
    ("pylon", "traffic_cone"),
];

/// Leading instruction fragments stripped before matching.
/// Longest-first effect via loop; keep lowercase with trailing space.
const LEAD_STRIP: &[&str] = &[
    "please select all images with ",
    "please select all pictures with ",
    "please select all images containing ",
    "please select all pictures containing ",
    "please click on each image containing ",
    "please click on each picture containing ",
    "please click each image containing ",
    "please click each picture containing ",
    "select all images containing ",
    "select all pictures containing ",
    "select all images with ",
    "select all pictures with ",
    "select all images of ",
    "select all pictures of ",
    "click each image containing ",
    "click each picture containing ",
    "click each image with ",
    "click on each ",
    "click on ",
    "select every ",
    "select all ",
    "check all images with ",
    "check all ",
    "find all ",
    "find each ",
    "choose each ",
    "click each ",
    "click ",
    "choose ",
    "tap each ",
    "please ",
];

/// Map a raw alias to its canonical label.
///
/// Compares case-insensitively; underscores and hyphens count as spaces.
/// Unknown input is returned unchanged.
pub fn canonical_label(raw: &str) -> &str {
    let norm = raw.trim().replace(['_', '-'], " ");
    let q = norm.trim();
    for (alias, canon) in ALIASES {
        if q.eq_ignore_ascii_case(alias) {
            return canon;
        }
    }
    // Direct hit on a canonical key in either separator style.
    for label in LABELS {
        let spaced: String = label.replace('_', " ");
        if q.eq_ignore_ascii_case(label) || q.eq_ignore_ascii_case(&spaced) {
            return label;
        }
    }
    raw
}

/// Normalize a widget prompt to a canonical label.
///
/// Lowercases, strips polite prefixes ("please ..."), instruction verbs
/// ("select all", "click each ..."), articles and trailing punctuation,
/// then maps aliases ("lorry" -> "truck", "traffic lights" ->
/// "traffic_light"). Output is `snake_case` for multi-word labels.
pub fn normalize_prompt(raw: &str) -> String {
    let mut s = raw.trim().to_lowercase().replace(['_', '-'], " ");
    // Collapse whitespace first.
    s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    // Strip leading instruction fragments, longest first effect via loop.
    loop {
        let mut cut = false;
        for lead in LEAD_STRIP {
            if let Some(rest) = s.strip_prefix(lead) {
                s = rest.to_string();
                cut = true;
                break;
            }
        }
        if !cut {
            break;
        }
    }
    // Strip leading articles left over after verb removal.
    for art in ["a ", "an ", "the "] {
        if let Some(rest) = s.strip_prefix(art) {
            s = rest.to_string();
            break;
        }
    }
    // Strip trailing punctuation.
    s = s
        .trim_end_matches(['.', '!', '?', ':', ';', ')'])
        .trim()
        .to_string();
    // Direct alias/canonical hit first (covers multi-word keys).
    let direct = canonical_or_self(&s);
    if LABELS.contains(&direct.as_str()) {
        return direct;
    }
    // Simple plural back-off: "buses" -> "bus", "traffic lights" ->
    // "traffic light" (then alias). Try whole-string, then last-word-only
    // so "stop signs" still lands without an explicit plural alias.
    if let Some(stripped) = s.strip_suffix('s') {
        let cand = canonical_or_self(stripped);
        if LABELS.contains(&cand.as_str()) {
            return cand;
        }
    }
    if let Some(stripped) = s.strip_suffix("es") {
        let cand = canonical_or_self(stripped);
        if LABELS.contains(&cand.as_str()) {
            return cand;
        }
    }
    // Last-word singularization for two-word labels missing a plural alias.
    if let Some(sp) = s.rfind(' ') {
        let (head, tail) = s.split_at(sp);
        let tail = tail.trim_start();
        for cand_tail in [
            tail.strip_suffix('s').unwrap_or(tail),
            tail.strip_suffix("es").unwrap_or(tail),
        ] {
            let cand = canonical_or_self(&format!("{head} {cand_tail}"));
            if LABELS.contains(&cand.as_str()) {
                return cand;
            }
        }
    }
    direct
}

fn canonical_or_self(s: &str) -> String {
    canonical_label(s).to_string()
}

/// Per-label decision threshold in 0..=1.
///
/// Common, well-trained categories sit lower; rare / confusable scene
/// labels sit higher (fail-closed). Unknown labels use the strict default
/// 0.80 — low confidence returns no click, never a guess.
pub fn threshold_for(label: &str) -> f32 {
    // Accept both `traffic_light` and `traffic light` spellings.
    let key = label.to_lowercase().replace(['-', ' '], "_");
    match canonical_label(&key) {
        // High-frequency, best-trained.
        "car" | "dog" | "cat" => 0.55,
        // Common vehicles + common street prompts.
        "truck" | "bus" | "motorcycle" | "bicycle" | "airplane" | "train" | "boat" | "bird"
        | "horse" | "pedestrian" | "traffic_light" => 0.60,
        // Medium frequency.
        "cow" | "sheep" | "lion" | "tiger" | "bear" | "elephant" | "zebra" | "giraffe"
        | "monkey" | "taxi" | "bridge" | "building" | "tree" | "road" | "crosswalk"
        | "stop_sign" => 0.65,
        // Confusable / rarer.
        "helicopter" | "tram" | "rabbit" | "deer" | "frog" | "fish" | "mountain" | "water"
        | "tower" | "statue" | "fountain" | "traffic_cone" | "street_light" => 0.70,
        // Rarest scene props — strict.
        "shark" | "whale" | "snake" | "tunnel" | "fire_hydrant" | "bicycle_rack"
        | "motorbus_stop" => 0.75,
        _ => 0.80,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_instructions_and_articles() {
        assert_eq!(
            normalize_prompt("Please select all images with cars."),
            "car"
        );
        assert_eq!(normalize_prompt("Click each image containing a bus"), "bus");
        assert_eq!(normalize_prompt("select all aeroplanes"), "airplane");
    }

    #[test]
    fn unknown_label_uses_strict_default() {
        assert_eq!(threshold_for("car"), 0.55);
        assert_eq!(threshold_for("something-new"), 0.80);
    }

    #[test]
    fn top50_labels_are_canonical() {
        assert_eq!(LABELS.len(), 50);
        for l in [
            "traffic_light",
            "pedestrian",
            "taxi",
            "bridge",
            "motorbus_stop",
        ] {
            assert!(LABELS.contains(&l), "missing {l}");
        }
    }

    #[test]
    fn multiword_aliases_normalize() {
        assert_eq!(
            normalize_prompt("Please select all traffic lights"),
            "traffic_light"
        );
        assert_eq!(
            normalize_prompt("Click each image containing a bus stop"),
            "motorbus_stop"
        );
        assert_eq!(normalize_prompt("select all stop signs."), "stop_sign");
        assert_eq!(
            normalize_prompt("Please select all pedestrians"),
            "pedestrian"
        );
        assert_eq!(normalize_prompt("select all taxis"), "taxi");
    }

    #[test]
    fn thresholds_cover_new_labels_fail_closed() {
        assert_eq!(threshold_for("traffic_light"), 0.60);
        assert_eq!(threshold_for("traffic light"), 0.60);
        assert_eq!(threshold_for("pedestrian"), 0.60);
        assert_eq!(threshold_for("bridge"), 0.65);
        assert_eq!(threshold_for("motorbus_stop"), 0.75);
        assert_eq!(threshold_for("fire_hydrant"), 0.75);
        assert_eq!(threshold_for("definitely-not-a-label"), 0.80);
        for l in LABELS {
            let t = threshold_for(l);
            assert!((0.0..=1.0).contains(&t), "out of range for {l}");
        }
    }
}
