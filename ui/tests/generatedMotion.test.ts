import assert from "node:assert/strict";
import test from "node:test";
import { AnimationMixer, Vector3 } from "three";
import { generatedMotion } from "../src/generatedMotion";

const bvh = `HIERARCHY
ROOT Root
{
 OFFSET 0 0 0
 CHANNELS 6 Xposition Yposition Zposition Zrotation Yrotation Xrotation
 JOINT Hips
 {
  OFFSET 0 100 0
  CHANNELS 3 Zrotation Yrotation Xrotation
  JOINT Head
  {
   OFFSET 0 50 0
   CHANNELS 3 Zrotation Yrotation Xrotation
   End Site
   {
    OFFSET 0 10 0
   }
  }
 }
}
MOTION
Frames: 2
Frame Time: 0.0333333333333333
0 0 0 0 0 0 0 0 0 0 0 0
60 0 0 0 0 0 0 0 0 0 0 0
`;

test("generated BVH playback preserves root displacement in metres", () => {
  const source = generatedMotion(bvh);
  const mixer = new AnimationMixer(source.object);
  mixer.clipAction(source.animations[0]).play();
  mixer.setTime(1 / 60);
  source.object.updateMatrixWorld(true);
  const hips = source.object.getObjectByName("Hips")!;
  const position = hips.getWorldPosition(new Vector3());
  assert.ok(Math.abs(position.x - 0.3) < 1e-5);
  assert.ok(Math.abs(position.y - 1) < 1e-5);
  assert.ok(source.bounds.radius > 0.3 && source.bounds.radius < 2);
  source.dispose();
});

test("invalid motion samples cannot reach the viewport", () => {
  assert.throws(() => generatedMotion(bvh.replace("60 0 0 0 0 0 0 0 0 0 0 0", "NaN 0 0 0 0 0 0 0 0 0 0 0")), /invalid animation samples/);
});

test("Kimodo absolute Hips channels do not add a second metre of height", () => {
  const absolute = bvh.replace("  CHANNELS 3 Zrotation Yrotation Xrotation", "  CHANNELS 6 Xposition Yposition Zposition Zrotation Yrotation Xrotation")
    .replace("\n0 0 0 0 0 0 0 0 0 0 0 0\n", "\n0 0 0 0 0 0 0 100 0 0 0 0 0 0 0\n")
    .replace("\n60 0 0 0 0 0 0 0 0 0 0 0\n", "\n60 0 0 0 0 0 0 100 0 0 0 0 0 0 0\n");
  const source = generatedMotion(absolute);
  const mixer = new AnimationMixer(source.object);
  mixer.clipAction(source.animations[0]).play();
  mixer.setTime(1 / 60);
  source.object.updateMatrixWorld(true);
  const hips = source.object.getObjectByName("Hips")!.getWorldPosition(new Vector3());
  assert.ok(Math.abs(hips.y - 1) < 1e-5);
  assert.ok(Math.abs(hips.x - 0.3) < 1e-5);
  source.dispose();
});
