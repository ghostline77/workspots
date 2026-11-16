import { AnimationMixer, Bone, Box3, CylinderGeometry, Group, Mesh, MeshStandardMaterial, SphereGeometry, Vector3 } from "three";
import { BVHLoader } from "three/addons/loaders/BVHLoader.js";

/** Show source SOMA motion as a mannequin; no NPC rig mapping is implied. */
export function sourceMotion(text: string) {
  const { skeleton, clip } = new BVHLoader().parse(text);
  // Kimodo writes absolute Hips translations alongside a nominal rest offset.
  // BVHLoader adds that offset again; remove it from the resulting track only
  // when this source declares position channels on Hips.
  if (/\bJOINT\s+Hips\s*\{[^{}]*\bCHANNELS\s+6\s+Xposition\s+Yposition\s+Zposition/.test(text)) {
    const hips = skeleton.bones.find(bone => bone.name === "Hips");
    const track = clip.tracks.find(track => track.name === "Hips.position");
    if (hips && track) {
      for (let i = 0; i < track.values.length; i += 3) {
        track.values[i] -= hips.position.x;
        track.values[i + 1] -= hips.position.y;
        track.values[i + 2] -= hips.position.z;
      }
    }
  }
  if (!clip.tracks.length || !Number.isFinite(clip.duration) || clip.duration <= 0 ||
      clip.tracks.some(track => Array.from(track.values).some(value => !Number.isFinite(value)))) {
    throw new Error("The motion contains invalid animation samples.");
  }
  return { skeleton, clip };
}

export function generatedMotion(text: string) {
  const { skeleton, clip } = sourceMotion(text);
  const root = new Group();
  root.name = "SOMA source motion";
  root.scale.setScalar(0.01); // Kimodo's standard-rest BVH exports centimetres.
  root.add(skeleton.bones[0]);
  const sphere = new SphereGeometry(1, 12, 8);
  const cylinder = new CylinderGeometry(1, 1, 1, 10);
  const body = new MeshStandardMaterial({ color: "#a9b5b0", roughness: 0.65, metalness: 0.12 });
  const joints = new MeshStandardMaterial({ color: "#c7f07b", roughness: 0.6 });
  const up = new Vector3(0, 1, 0);
  for (const bone of skeleton.bones) {
    if (bone.name === "Root" || bone.name === "ENDSITE") continue;
    const fine = /thumb|index|middle|ring|pinky|finger/i.test(bone.name);
    const joint = new Mesh(sphere, joints);
    joint.name = `${bone.name} marker`;
    joint.scale.setScalar(fine ? 0.7 : 1.8);
    bone.add(joint);
    if (bone.name.toLowerCase() === "head") {
      const head = new Mesh(sphere, body);
      head.position.y = 7;
      head.scale.set(7, 9, 7);
      bone.add(head);
    }
    if (!(bone.parent instanceof Bone) || bone.parent.name === "Root") continue;
    const direction = bone.position.clone();
    const length = direction.length();
    if (length < 0.01 || !Number.isFinite(length)) continue;
    const limb = new Mesh(cylinder, body);
    limb.name = `${bone.name} segment`;
    const radius = fine ? 0.65 : Math.min(3.7, Math.max(1.3, length * 0.09));
    limb.scale.set(radius, length, radius);
    limb.position.copy(direction).multiplyScalar(0.5);
    limb.quaternion.setFromUnitVectors(up, direction.normalize());
    bone.parent.add(limb);
  }
  clip.name = "Generated motion";
  const mixer = new AnimationMixer(root);
  mixer.clipAction(clip).play();
  const bounds = new Box3();
  for (let i = 0; i <= 8; i++) {
    mixer.setTime(clip.duration * i / 8);
    root.updateMatrixWorld(true);
    bounds.expandByObject(root);
  }
  mixer.stopAllAction(); mixer.uncacheRoot(root);
  root.updateMatrixWorld(true);
  const center = bounds.getCenter(new Vector3());
  return { object: root, animations: [clip], bounds: {
    center: [center.x, center.y, center.z] as [number, number, number],
    radius: Math.max(0.1, bounds.getSize(new Vector3()).length() / 2),
  }, dispose() {
    sphere.dispose(); cylinder.dispose(); body.dispose(); joints.dispose();
  } };
}
