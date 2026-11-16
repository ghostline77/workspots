import { AnimationClip, Box3, Matrix4, QuaternionKeyframeTrack, Vector3, VectorKeyframeTrack, type KeyframeTrack, type Object3D } from "three";
import type { GLTF } from "three/addons/loaders/GLTFLoader.js";

type Clip = { format:string; version:number; times:number[]; duration:number;
  rest:{name:string; local:number[]}[]; scene_rotation_y:number;
  tracks:{node:number; rotations:number[][]; positions?:number[][]}[];
  bounds:{center:[number,number,number];radius:number} };

/** Bind the backend's native local tracks to loader-assigned object names. No retarget math runs here. */
export function nativeGeneratedClip(gltf:GLTF, clip:Clip) {
  if (clip.format !== "ghostline-retargeted-motion" || clip.version !== 1) throw new Error("Unsupported native motion clip.");
  const objects = new Map<number,Object3D>();
  gltf.scene.rotation.y = clip.scene_rotation_y;
  gltf.scene.traverse(object => {
    const index = gltf.parser.associations.get(object)?.nodes;
    if (index === undefined) return;
    const rest = clip.rest[index];
    if (!rest || rest.local.length !== 16 || !rest.local.every(Number.isFinite)) throw new Error("Native rest pose does not match the scene.");
    new Matrix4().fromArray(rest.local).decompose(object.position,object.quaternion,object.scale);
    objects.set(index,object);
  });
  const tracks:KeyframeTrack[] = [];
  for (const track of clip.tracks) {
    const object = objects.get(track.node);
    if (!object) throw new Error("Native motion references a missing character node.");
    tracks.push(new QuaternionKeyframeTrack(`${object.name}.quaternion`,clip.times,track.rotations.flat()));
    if (track.positions) tracks.push(new VectorKeyframeTrack(`${object.name}.position`,clip.times,track.positions.flat()));
  }
  gltf.scene.updateMatrixWorld(true);
  return { object:gltf.scene, animations:[new AnimationClip("Generated motion",clip.duration,tracks)], bounds:clip.bounds,
    dispose(){ disposeCharacter(gltf.scene); } };
}
export function disposeCharacter(object: Object3D) {
  const resources = new Set<{ dispose():void }>();
  object.traverse((node:any) => {
    if (!node.isMesh) return;
    resources.add(node.geometry);
    for (const material of Array.isArray(node.material) ? node.material : [node.material]) {
      resources.add(material);
      for (const value of Object.values(material) as any[]) if (value?.isTexture) resources.add(value);
    }
    if (node.isSkinnedMesh) resources.add(node.skeleton);
  });
  for (const resource of resources) resource.dispose();
}
