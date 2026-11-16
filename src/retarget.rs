//! SOMA BVH to native NPC rest-pose retargeting. Outputs metre-space local tracks.
use anyhow::{Context, Result, bail, ensure};
use glam::{Mat3, Mat4, Quat, Vec3};
use serde::Serialize;
use serde_json::Value;

#[derive(Clone)]
struct Joint {
    name: String,
    parent: Option<usize>,
    position: Vec3,
    rotation: Quat,
    channels: Vec<String>,
}
#[derive(Clone, Copy)]
struct Pose {
    position: Vec3,
    rotation: Quat,
}

fn world(joints: &[Joint], outer: Quat) -> Vec<Pose> {
    let mut result: Vec<Pose> = Vec::with_capacity(joints.len());
    for joint in joints {
        let parent = joint.parent.map_or(
            Pose {
                position: Vec3::ZERO,
                rotation: outer,
            },
            |p| result[p],
        );
        result.push(Pose {
            position: parent.position + parent.rotation * joint.position,
            rotation: (parent.rotation * joint.rotation).normalize(),
        });
    }
    result
}

struct Bvh {
    joints: Vec<Joint>,
    samples: Vec<Vec<f32>>,
    dt: f32,
}
struct Tokens<'a>(std::str::SplitWhitespace<'a>);
impl<'a> Tokens<'a> {
    fn next(&mut self) -> Result<&'a str> {
        self.0.next().context("Unexpected end of BVH")
    }
    fn expect(&mut self, expected: &str) -> Result<()> {
        ensure!(self.next()? == expected, "Expected BVH {expected}");
        Ok(())
    }
    fn number(&mut self) -> Result<f32> {
        let n: f32 = self.next()?.parse()?;
        ensure!(n.is_finite(), "Nonfinite BVH sample");
        Ok(n)
    }
    fn joint(
        &mut self,
        joints: &mut Vec<Joint>,
        parent: Option<usize>,
        depth: usize,
        end: bool,
    ) -> Result<()> {
        ensure!(
            depth < 128 && joints.len() < 1024,
            "BVH hierarchy exceeds limits"
        );
        let name = if end {
            self.expect("Site")?;
            "ENDSITE"
        } else {
            self.next()?
        }
        .to_owned();
        self.expect("{")?;
        self.expect("OFFSET")?;
        let position = Vec3::new(self.number()?, self.number()?, self.number()?) * 0.01;
        let index = joints.len();
        joints.push(Joint {
            name,
            parent,
            position,
            rotation: Quat::IDENTITY,
            channels: Vec::new(),
        });
        loop {
            match self.next()? {
                "CHANNELS" => {
                    let count: usize = self.next()?.parse()?;
                    ensure!(count <= 6, "Too many BVH channels");
                    for _ in 0..count {
                        let channel = self.next()?;
                        ensure!(
                            [
                                "Xposition",
                                "Yposition",
                                "Zposition",
                                "Xrotation",
                                "Yrotation",
                                "Zrotation"
                            ]
                            .contains(&channel),
                            "Unknown BVH channel"
                        );
                        joints[index].channels.push(channel.to_owned());
                    }
                }
                "JOINT" => self.joint(joints, Some(index), depth + 1, false)?,
                "End" => self.joint(joints, Some(index), depth + 1, true)?,
                "}" => break,
                other => bail!("Unexpected BVH token {other}"),
            }
        }
        Ok(())
    }
}
fn parse(text: &str) -> Result<Bvh> {
    let mut tokens = Tokens(text.split_whitespace());
    tokens.expect("HIERARCHY")?;
    tokens.expect("ROOT")?;
    let mut joints = Vec::new();
    tokens.joint(&mut joints, None, 0, false)?;
    tokens.expect("MOTION")?;
    tokens.expect("Frames:")?;
    let frames: usize = tokens.next()?.parse()?;
    ensure!((2..=3600).contains(&frames), "Unsupported frame count");
    tokens.expect("Frame")?;
    tokens.expect("Time:")?;
    let dt = tokens.number()?;
    ensure!((dt - 1.0 / 30.0).abs() < 1e-6, "Expected 30 FPS BVH");
    let channels: usize = joints.iter().map(|j| j.channels.len()).sum();
    ensure!(
        channels > 0 && frames * channels <= 4_000_000,
        "BVH samples exceed limits"
    );
    let samples = (0..frames)
        .map(|_| (0..channels).map(|_| tokens.number()).collect())
        .collect::<Result<Vec<Vec<f32>>>>()?;
    ensure!(tokens.0.next().is_none(), "Trailing BVH data");
    Ok(Bvh {
        joints,
        samples,
        dt,
    })
}

fn source_name(name: &str) -> Option<String> {
    let special = match name {
        "Spine" => "Spine1",
        "Spine1" => "Spine2",
        "Spine2" | "Spine3" => "Chest",
        "Neck" => "Neck1",
        "Neck1" => "Neck2",
        "LeftUpLeg" => "LeftLeg",
        "RightUpLeg" => "RightLeg",
        "LeftLeg" => "LeftShin",
        "RightLeg" => "RightShin",
        _ => "",
    };
    if !special.is_empty() {
        return Some(special.to_owned());
    }
    for side in ["Left", "Right"] {
        for digit in ["Thumb", "Index", "Middle", "Ring", "Pinky"] {
            if name == format!("{side}InHand{digit}") {
                return Some(format!("{side}Hand{digit}1"));
            }
            for i in 1..=3 {
                if name == format!("{side}Hand{digit}{i}") {
                    return Some(format!("{side}Hand{digit}{}", i + 1));
                }
            }
        }
    }
    if ["Hips", "Head"].contains(&name)
        || ["Left", "Right"].iter().any(|s| {
            ["Shoulder", "Arm", "ForeArm", "Hand", "Foot", "ToeBase"]
                .iter()
                .any(|p| name == format!("{s}{p}"))
        })
    {
        Some(name.to_owned())
    } else {
        None
    }
}
fn next_name(name: &str, native: bool) -> Option<String> {
    let spine = if native {
        &[
            "Hips", "Spine", "Spine1", "Spine2", "Spine3", "Neck", "Neck1", "Head",
        ][..]
    } else {
        &[
            "Hips", "Spine1", "Spine2", "Chest", "Neck1", "Neck2", "Head", "HeadEnd",
        ][..]
    };
    if let Some(i) = spine.iter().position(|n| *n == name) {
        return spine.get(i + 1).map(|s| (*s).to_owned());
    }
    for side in ["Left", "Right"] {
        let Some(part) = name.strip_prefix(side) else {
            continue;
        };
        let pairs = if native {
            &[
                ("Shoulder", "Arm"),
                ("Arm", "ForeArm"),
                ("ForeArm", "Hand"),
                ("Hand", "InHandMiddle"),
                ("UpLeg", "Leg"),
                ("Leg", "Foot"),
                ("Foot", "ToeBase"),
            ][..]
        } else {
            &[
                ("Shoulder", "Arm"),
                ("Arm", "ForeArm"),
                ("ForeArm", "Hand"),
                ("Hand", "HandMiddle1"),
                ("Leg", "Shin"),
                ("Shin", "Foot"),
                ("Foot", "ToeBase"),
                ("ToeBase", "ToeEnd"),
            ][..]
        };
        if let Some((_, next)) = pairs.iter().find(|(p, _)| *p == part) {
            return Some(format!("{side}{next}"));
        }
        for digit in ["Thumb", "Index", "Middle", "Ring", "Pinky"] {
            if part == format!("InHand{digit}") {
                return Some(format!("{side}Hand{digit}1"));
            }
            for i in 1..=4 {
                if part == format!("Hand{digit}{i}") {
                    if !native && (i == 4 || (digit == "Thumb" && i == 3)) {
                        return Some(format!("{side}Hand{digit}End"));
                    }
                    return Some(format!("{side}Hand{digit}{}", i + 1));
                }
            }
        }
    }
    None
}
fn frame(index: usize, joints: &[Joint], poses: &[Pose], native: bool) -> Quat {
    let joint = &joints[index];
    let next = next_name(&joint.name, native).and_then(|name| {
        (index + 1..joints.len()).find(|i| {
            if joints[*i].name != name {
                return false;
            }
            let mut parent = joints[*i].parent;
            while let Some(p) = parent {
                if p == index {
                    return true;
                }
                parent = joints[p].parent;
            }
            false
        })
    });
    let position = poses[index].position;
    let mut direction = next
        .map(|i| poses[i].position - position)
        .or_else(|| joint.parent.map(|p| position - poses[p].position))
        .unwrap_or(Vec3::Y)
        .normalize_or(Vec3::Y);
    if native && next.is_none() && joint.name.contains("Hand") && joint.name.ends_with(['2', '3']) {
        // Native terminal phalanges have no tip joint. Their authored X axis
        // points towards the fingertip (mirrored on the right hand); using the
        // preceding segment retains the rest pose's distal curl in the mesh.
        direction = poses[index].rotation
            * if joint.name.starts_with("Right") {
                -Vec3::X
            } else {
                Vec3::X
            };
    }
    let mut reference = if joint.name.contains("Foot") || joint.name.contains("Toe") {
        Vec3::Y
    } else {
        Vec3::Z
    };
    if ["LeftArm", "RightArm", "LeftForeArm", "RightForeArm"].contains(&joint.name.as_str()) {
        let side = if joint.name.starts_with("Left") {
            "Left"
        } else {
            "Right"
        };
        let find = |part: &str| {
            joints
                .iter()
                .position(|j| j.name == format!("{side}{part}"))
        };
        if let (Some(arm), Some(elbow), Some(hand)) = (find("Arm"), find("ForeArm"), find("Hand")) {
            let bend = (poses[elbow].position - poses[arm].position)
                .cross(poses[hand].position - poses[elbow].position);
            let fallback = if side == "Right" { Vec3::Y } else { -Vec3::Y };
            reference = if bend.length_squared() > 1e-8 {
                let normal = bend.normalize();
                if !native && normal.dot(poses[arm].rotation * fallback) < 0. {
                    -normal
                } else {
                    normal
                }
            } else {
                fallback
            };
        }
    }
    // A shared palm normal fixes axial roll consistently for the wrist and all
    // digits. A world-axis fallback can flip the finger bend plane across rigs.
    if joint.name.contains("Hand") {
        let side = if joint.name.starts_with("Left") {
            "Left"
        } else {
            "Right"
        };
        let mut hand = Some(index);
        while let Some(i) = hand {
            if joints[i].name == format!("{side}Hand") {
                break;
            }
            hand = joints[i].parent;
        }
        if let Some(hand) = hand {
            let digit = if native { "1" } else { "2" };
            let find = |part: &str| {
                (hand + 1..joints.len())
                    .find(|i| joints[*i].name == format!("{side}Hand{part}{digit}"))
            };
            if let (Some(index), Some(middle), Some(pinky)) =
                (find("Index"), find("Middle"), find("Pinky"))
            {
                let along = (poses[middle].position - poses[hand].position).normalize_or(Vec3::Y);
                reference = (poses[index].position - poses[pinky].position)
                    .cross(along)
                    .normalize_or(reference);
                if joint.name == format!("{side}Hand") {
                    direction = along;
                }
            }
        }
    }
    if direction.dot(reference).abs() > 0.97 {
        reference = [Vec3::X, Vec3::Y, Vec3::Z]
            .into_iter()
            .min_by(|a, b| direction.dot(*a).abs().total_cmp(&direction.dot(*b).abs()))
            .unwrap();
    }
    let right = direction.cross(reference).normalize();
    Quat::from_mat3(&Mat3::from_cols(
        right,
        direction,
        right.cross(direction).normalize(),
    ))
    .normalize()
}

#[derive(Serialize)]
struct Track {
    node: usize,
    rotations: Vec<[f32; 4]>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    positions: Vec<[f32; 3]>,
}

/// Returns native local quaternion/hip tracks, addressed by GLB node index.
#[cfg(test)]
pub fn bake(text: &str, rest: &Value, glb: &Value) -> Result<Value> {
    bake_with_hands(text, rest, glb, "generated")
}

/// Optional explicit hand-pose edit; source media remains unchanged.
pub fn bake_with_hands(text: &str, rest: &Value, glb: &Value, hand_pose: &str) -> Result<Value> {
    ensure!(
        ["generated", "open_palm", "open_right", "open_left"].contains(&hand_pose),
        "Unknown hand pose"
    );
    let bvh = parse(text)?;
    let nodes = glb["nodes"].as_array().context("Missing GLB nodes")?;
    let rest_nodes = rest["nodes"]
        .as_array()
        .context("Missing native rest nodes")?;
    ensure!(
        nodes.len() == rest_nodes.len() && nodes.len() <= 4096,
        "Rest hierarchy mismatch"
    );
    let mut parents = vec![None; nodes.len()];
    for (i, node) in nodes.iter().enumerate() {
        if let Some(children) = node["children"].as_array() {
            for child in children {
                let c = child.as_u64().context("Invalid node index")? as usize;
                ensure!(
                    c < nodes.len() && c != i && parents[c].is_none(),
                    "Invalid node parent"
                );
                parents[c] = Some(i);
            }
        }
    }
    // GLB indices need not be topological. Retarget internal joints are reordered,
    // and the published tracks keep their original GLB indices.
    let mut order = Vec::new();
    let mut pending = vec![true; nodes.len()];
    while order.len() < nodes.len() {
        let before = order.len();
        for i in 0..nodes.len() {
            if pending[i] && parents[i].is_none_or(|p| !pending[p]) {
                pending[i] = false;
                order.push(i);
            }
        }
        ensure!(order.len() > before, "Cyclic native hierarchy");
    }
    let mut inverse = vec![0; nodes.len()];
    for (i, n) in order.iter().enumerate() {
        inverse[*n] = i;
    }
    let mut target = Vec::new();
    for n in &order {
        let node = &rest_nodes[*n];
        ensure!(node["name"] == nodes[*n]["name"], "Rest node name mismatch");
        let values: [f32; 16] = serde_json::from_value(node["local"].clone())?;
        ensure!(
            values.iter().all(|x| x.is_finite()),
            "Nonfinite rest transform"
        );
        let (scale, rotation, position) =
            Mat4::from_cols_array(&values).to_scale_rotation_translation();
        ensure!(
            scale.is_finite() && scale.abs_diff_eq(Vec3::ONE, 1e-3) && rotation.is_finite(),
            "Unsupported scaled native rig"
        );
        target.push(Joint {
            name: node["name"]
                .as_str()
                .context("Missing joint name")?
                .to_owned(),
            parent: parents[*n].map(|p| inverse[p]),
            position,
            rotation,
            channels: Vec::new(),
        });
    }
    let outer = Quat::from_rotation_y(std::f32::consts::PI);
    let target_rest = world(&target, outer);
    let source_rest = world(&bvh.joints, Quat::IDENTITY);
    let mut mapping: Vec<(usize, usize, Quat)> = Vec::new();
    for (i, joint) in target.iter().enumerate() {
        if let Some(s) =
            source_name(&joint.name).and_then(|name| bvh.joints.iter().position(|j| j.name == name))
        {
            // Preserve native neutral curvature and clavicle shape. Only limb
            // chains need A-pose to source T-pose anatomical alignment.
            let alignment = if [
                "Hips",
                "Spine",
                "Spine1",
                "Spine2",
                "Spine3",
                "Neck",
                "Neck1",
                "Head",
                "LeftShoulder",
                "RightShoulder",
            ]
            .contains(&joint.name.as_str())
            {
                Quat::IDENTITY
            } else {
                frame(s, &bvh.joints, &source_rest, false)
                    * frame(i, &target, &target_rest, true).inverse()
            };
            // Linked facial/hair rigs may omit the limb below a shoulder. Never
            // infer a new anatomical axis from their truncated hierarchy: their
            // skinned surfaces must receive the same world delta as the body.
            let correction = if let Some((primary, _, correction)) = mapping
                .iter()
                .find(|(primary, _, _)| target[*primary].name == joint.name)
            {
                *correction * target_rest[*primary].rotation.inverse() * target_rest[i].rotation
            } else {
                source_rest[s].rotation.inverse() * alignment * target_rest[i].rotation
            };
            mapping.push((i, s, correction));
        }
    }
    for required in [
        "Hips",
        "Head",
        "LeftArm",
        "RightArm",
        "LeftLeg",
        "RightLeg",
        "LeftFoot",
        "RightFoot",
        "LeftHand",
        "RightHand",
    ] {
        ensure!(
            mapping.iter().any(|(i, _, _)| target[*i].name == required),
            "Missing required native joint {required}"
        );
    }
    let find = |joints: &[Joint], name: &str| {
        joints
            .iter()
            .position(|j| j.name == name)
            .with_context(|| format!("Missing {name}"))
    };
    let th = find(&target, "Hips")?;
    let tf = find(&target, "LeftFoot")?;
    let sh = find(&bvh.joints, "Hips")?;
    let sf = find(&bvh.joints, "LeftFoot")?;
    let root_scale = (target_rest[th].position.y - target_rest[tf].position.y)
        / (source_rest[sh].position.y - source_rest[sf].position.y);
    ensure!(
        root_scale.is_finite() && (0.5..=2.).contains(&root_scale),
        "Unsupported character proportions"
    );
    let mut tracks: Vec<Track> = mapping
        .iter()
        .map(|(i, _, _)| Track {
            node: order[*i],
            rotations: Vec::with_capacity(bvh.samples.len()),
            positions: Vec::new(),
        })
        .collect();
    // Facial rigs can begin at Spine3 under an independent component root.
    // Their root translation must follow the corresponding body joint as well
    // as its rotation. Otherwise face/neck geometry separates during body motion.
    let attachments: Vec<_> = target
        .iter()
        .enumerate()
        .map(|(i, joint)| {
            let detached = joint
                .parent
                .is_none_or(|p| source_name(&target[p].name).is_none());
            if detached && joint.name != "Hips" {
                target[..i].iter().position(|other| {
                    other.name == joint.name && source_name(&other.name).is_some()
                })
            } else {
                None
            }
        })
        .collect();
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for sample in &bvh.samples {
        let mut source = bvh.joints.clone();
        let mut cursor = 0;
        for joint in &mut source {
            // Kimodo Hips position channels are absolute, not offset-relative.
            if joint.name == "Hips" && joint.channels.iter().any(|c| c.ends_with("position")) {
                joint.position = Vec3::ZERO;
            }
            for channel in &joint.channels {
                let finger = joint.name.contains("Hand")
                    && !["LeftHand", "RightHand"].contains(&joint.name.as_str());
                let value = if hand_pose != "generated"
                    && finger
                    && (hand_pose == "open_palm"
                        || (hand_pose == "open_right" && joint.name.starts_with("Right"))
                        || (hand_pose == "open_left" && joint.name.starts_with("Left")))
                    && channel.ends_with("rotation")
                {
                    0.
                } else {
                    sample[cursor]
                };
                cursor += 1;
                match channel.as_str() {
                    "Xposition" => joint.position.x += value * 0.01,
                    "Yposition" => joint.position.y += value * 0.01,
                    "Zposition" => joint.position.z += value * 0.01,
                    "Xrotation" => joint.rotation *= Quat::from_rotation_x(value.to_radians()),
                    "Yrotation" => joint.rotation *= Quat::from_rotation_y(value.to_radians()),
                    "Zrotation" => joint.rotation *= Quat::from_rotation_z(value.to_radians()),
                    _ => unreachable!(),
                }
            }
        }
        let source_pose = world(&source, Quat::IDENTITY);
        let mut posed = target.clone();
        let mut target_pose = world(&posed, outer);
        for (track, (i, s, correction)) in tracks.iter_mut().zip(&mapping) {
            let positional = target[*i].name.contains("Hand")
                || ["LeftArm", "RightArm", "LeftForeArm", "RightForeArm"]
                    .contains(&target[*i].name.as_str());
            let desired = if positional {
                // Transfer the posed bend/palm frame, not arbitrary source axial
                // roll. The correction still retains each native skin-bone axis.
                frame(*s, &source, &source_pose, false)
                    * frame(*s, &bvh.joints, &source_rest, false).inverse()
                    * source_rest[*s].rotation
                    * *correction
            } else {
                source_pose[*s].rotation * *correction
            };
            let parent = posed[*i].parent.map_or(
                Pose {
                    position: Vec3::ZERO,
                    rotation: outer,
                },
                |p| target_pose[p],
            );
            posed[*i].rotation = (parent.rotation.inverse() * desired).normalize();
            if posed[*i].name == "Hips" {
                let position = target_rest[*i].position
                    + (source_pose[sh].position - source_rest[sh].position) * root_scale;
                posed[*i].position = parent.rotation.inverse() * (position - parent.position);
                track.positions.push(posed[*i].position.to_array());
            } else if let Some(anchor) = attachments[*i] {
                let delta = target_pose[anchor].rotation * target_rest[anchor].rotation.inverse();
                let position = target_pose[anchor].position
                    + delta * (target_rest[*i].position - target_rest[anchor].position);
                posed[*i].position = parent.rotation.inverse() * (position - parent.position);
                track.positions.push(posed[*i].position.to_array());
            }
            // Updating descendants also carries unmapped twist/helper rest bones.
            target_pose = world(&posed, outer);
            let rotation = posed[*i].rotation;
            ensure!(
                rotation.is_finite() && posed[*i].position.is_finite(),
                "Nonfinite retargeted pose"
            );
            track.rotations.push(rotation.to_array());
            min = min.min(target_pose[*i].position);
            max = max.max(target_pose[*i].position);
        }
    }
    Ok(
        serde_json::json!({"format":"ghostline-retargeted-motion","version":1,"algorithm":"soma-native-rest-v10","fps":30,
        "duration":(bvh.samples.len()-1) as f32*bvh.dt,"frames":bvh.samples.len(),"times":(0..bvh.samples.len()).map(|i|i as f32*bvh.dt).collect::<Vec<_>>(),
        "tracks":tracks,"rest":rest_nodes,"scene_rotation_y":std::f32::consts::PI,
        "bounds":{"center":((min+max)*0.5).to_array(),"radius":((max-min).length()*0.5+0.35)},
        "report":{"mapped_nodes":mapping.len(),"root_scale":root_scale},"hand_pose":hand_pose,"review_status":"unreviewed","runtime_verified":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finger_aim_does_not_copy_unobservable_axial_source_spin() {
        let joint = |name: &str, parent, position| Joint {
            name: name.into(),
            parent,
            position,
            rotation: Quat::IDENTITY,
            channels: vec![],
        };
        let mut joints = vec![
            joint("RightHand", None, Vec3::ZERO),
            joint("RightHandIndex2", Some(0), Vec3::new(-1., 0., 0.5)),
            joint("RightHandMiddle2", Some(0), -Vec3::X),
            joint("RightHandPinky2", Some(0), Vec3::new(-1., 0., -0.5)),
            joint("RightHandIndex4", Some(1), -Vec3::X),
            joint("RightHandIndexEnd", Some(4), -Vec3::X),
        ];
        let rest = world(&joints, Quat::IDENTITY);
        let expected = frame(4, &joints, &rest, false);
        joints[4].rotation = Quat::from_rotation_x(2.1);
        let posed = world(&joints, Quat::IDENTITY);
        assert!(rest[5].position.abs_diff_eq(posed[5].position, 1e-6));
        assert!(expected.dot(frame(4, &joints, &posed, false)).abs() > 0.99999);
    }
    #[test]
    fn terminal_finger_uses_authored_tip_axis_instead_of_rest_curl() {
        let joints = vec![
            Joint {
                name: "RightHandIndex2".into(),
                parent: None,
                position: Vec3::ZERO,
                rotation: Quat::IDENTITY,
                channels: vec![],
            },
            Joint {
                name: "RightHandIndex3".into(),
                parent: Some(0),
                position: -Vec3::X,
                rotation: Quat::from_rotation_z(0.7),
                channels: vec![],
            },
        ];
        let poses = world(&joints, Quat::IDENTITY);
        let direction = frame(1, &joints, &poses, true) * Vec3::Y;
        assert!(direction.dot(poses[1].rotation * -Vec3::X) > 0.9999);
        assert!(
            direction.dot(-Vec3::X) < 0.9,
            "Must remove the distal rest-curl bias"
        );
    }
    #[test]
    fn rejects_invalid_samples_and_cycles() {
        let text = "HIERARCHY ROOT Hips { OFFSET 0 100 0 CHANNELS 1 Yposition } MOTION Frames: 2 Frame Time: 0.033333333 100 NaN";
        assert!(parse(text).is_err());
        let text = text.replace("NaN", "110");
        assert_eq!(parse(&text).unwrap().samples.len(), 2);
        let nodes =
            serde_json::json!({"nodes":[{"name":"a","children":[1]},{"name":"b","children":[0]}]});
        let rest = serde_json::json!({"nodes":[{},{}]});
        assert!(
            bake(&text, &rest, &nodes)
                .unwrap_err()
                .to_string()
                .contains("Cyclic")
        );
    }

    /// Local asset acceptance test; requires the preserved native smoke-test scenes.
    #[test]
    #[ignore = "requires local Patch/Velvet source assets"]
    fn native_wave_acceptance() -> Result<()> {
        use std::{fs, io::Read, path::Path};
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("work");
        let text = fs::read_to_string(root.join("generated/a887835ed2316b54b83c9995/motion.bvh"))?;
        for (name, key, profile, stamp) in [
            (
                "patch",
                "e519f3eb0a1eb8d62ea20e08c4763ff6",
                "2a5cf5f753dc1fdeeacae23f",
                "1793987527986572500",
            ),
            (
                "velvet",
                "dcd5fbb3adf2e8df186c8fe33687840d",
                "b8fe50eeb58496c0ed0ecc1a",
                "1793987268476337000",
            ),
        ] {
            let rest: Value = serde_json::from_slice(&fs::read(
                root.join(format!("browser-workers/{profile}/{stamp}/assembly.json")),
            )?)?;
            let mut file = fs::File::open(root.join(format!("browser/{key}/scene.glb")))?;
            let mut header = [0; 20];
            file.read_exact(&mut header)?;
            let size = u32::from_le_bytes(header[12..16].try_into()?) as usize;
            let mut bytes = vec![0; size];
            file.read_exact(&mut bytes)?;
            let glb: Value = serde_json::from_slice(&bytes)?;
            let parsed = parse(&text)?;
            let neutral_values = parsed
                .joints
                .iter()
                .flat_map(|joint| {
                    joint.channels.iter().map(|channel| {
                        if joint.name == "Hips" && channel == "Yposition" {
                            "100"
                        } else {
                            "0"
                        }
                    })
                })
                .collect::<Vec<_>>()
                .join(" ");
            let hierarchy = text.split("MOTION").next().unwrap();
            let neutral = bake(
                &format!(
                    "{hierarchy}MOTION\nFrames: 2\nFrame Time: 0.033333333\n{neutral_values}\n{neutral_values}\n"
                ),
                &rest,
                &glb,
            )?;
            for track in neutral["tracks"].as_array().unwrap() {
                let node = track["node"].as_u64().unwrap() as usize;
                let name = glb["nodes"][node]["name"].as_str().unwrap();
                if [
                    "Hips",
                    "Spine",
                    "Spine1",
                    "Spine2",
                    "Spine3",
                    "Neck",
                    "Neck1",
                    "Head",
                    "LeftShoulder",
                    "RightShoulder",
                ]
                .contains(&name)
                {
                    let a: [f32; 16] =
                        serde_json::from_value(rest["nodes"][node]["local"].clone())?;
                    let expected = Mat4::from_cols_array(&a).to_scale_rotation_translation().1;
                    let q: [f32; 4] = serde_json::from_value(track["rotations"][0].clone())?;
                    ensure!(
                        Quat::from_array(q).dot(expected).abs() > 0.99999,
                        "Neutral {name} changes native body shape"
                    );
                }
            }
            let result = bake(&text, &rest, &glb)?;
            let open = bake_with_hands(&text, &rest, &glb, "open_right")?;
            for (generated, edited) in result["tracks"]
                .as_array()
                .unwrap()
                .iter()
                .zip(open["tracks"].as_array().unwrap())
            {
                let node = generated["node"].as_u64().unwrap() as usize;
                let name = glb["nodes"][node]["name"].as_str().unwrap();
                if !name.starts_with("Right") || !name.contains("Hand") {
                    ensure!(
                        generated == edited,
                        "Open right hand changes an unrelated joint"
                    );
                }
            }
            fs::write(
                root.join(format!("{name}-rust-wave-open.json")),
                serde_json::to_vec(&open)?,
            )?;
            ensure!(result["frames"] == 120, "Frame coverage mismatch");
            ensure!(
                result["report"]["mapped_nodes"].as_u64().unwrap_or(0) >= 50,
                "Missing body/finger mapping"
            );
            for track in result["tracks"].as_array().unwrap() {
                ensure!(
                    track["rotations"].as_array().unwrap().len() == 120,
                    "Truncated track"
                );
                for q in track["rotations"].as_array().unwrap() {
                    let q: [f32; 4] = serde_json::from_value(q.clone())?;
                    ensure!(
                        (Quat::from_array(q).length() - 1.).abs() < 1e-4,
                        "Invalid rotation"
                    );
                }
            }
            // Check linked rigs at the actual posed skin-driving joints. Facial
            // shoulders lack arm children and must not infer a different axis.
            let nodes = glb["nodes"].as_array().unwrap();
            let mut parents = vec![None; nodes.len()];
            for (i, node) in nodes.iter().enumerate() {
                if let Some(children) = node["children"].as_array() {
                    for c in children {
                        parents[c.as_u64().unwrap() as usize] = Some(i);
                    }
                }
            }
            let rest_matrices: Vec<_> = rest["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|n| {
                    let a: [f32; 16] = serde_json::from_value(n["local"].clone()).unwrap();
                    Mat4::from_cols_array(&a)
                })
                .collect();
            let matrix_world = |index: usize, locals: &[Mat4]| {
                let mut matrix = locals[index];
                let mut parent = parents[index];
                while let Some(p) = parent {
                    matrix = locals[p] * matrix;
                    parent = parents[p];
                }
                matrix
            };
            for f in [0, 21, 42, 84, 119] {
                let mut locals = rest_matrices.clone();
                for track in result["tracks"].as_array().unwrap() {
                    let node = track["node"].as_u64().unwrap() as usize;
                    let q: [f32; 4] = serde_json::from_value(track["rotations"][f].clone())?;
                    let (scale, _, mut position) = locals[node].to_scale_rotation_translation();
                    if track["positions"].is_array() {
                        position = Vec3::from_array(serde_json::from_value(
                            track["positions"][f].clone(),
                        )?);
                    }
                    locals[node] =
                        Mat4::from_scale_rotation_translation(scale, Quat::from_array(q), position);
                }
                let mut seen = std::collections::HashMap::new();
                for (i, node) in nodes.iter().enumerate() {
                    let name = node["name"].as_str().unwrap();
                    if source_name(name).is_none() {
                        continue;
                    }
                    let rest_q = matrix_world(i, &rest_matrices)
                        .to_scale_rotation_translation()
                        .1;
                    let q = matrix_world(i, &locals).to_scale_rotation_translation().1;
                    let delta = (q * rest_q.inverse()).normalize();
                    if let Some(primary) = seen.get(name) {
                        ensure!(
                            delta.abs_diff_eq(*primary, 0.001)
                                || delta.abs_diff_eq(-*primary, 0.001),
                            "Linked {name} rotation diverged at frame {f}"
                        );
                    } else {
                        seen.insert(name, delta);
                    }
                }
            }
            fs::write(
                root.join(format!("{name}-rust-wave.json")),
                serde_json::to_vec(&result)?,
            )?;
            println!("{name}: {}", result["report"]);
        }
        Ok(())
    }
}
