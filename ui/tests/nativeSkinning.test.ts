import assert from "node:assert/strict";
import test from "node:test";
import {
  Bone,
  BufferAttribute,
  BufferGeometry,
  Float32BufferAttribute,
  Group,
  Matrix4,
  MeshStandardMaterial,
  Skeleton,
  SkinnedMesh,
  Uint16BufferAttribute,
  Vector3,
  type Material,
  type WebGLRenderer,
} from "three";
import { nativeSkinning } from "../src/nativeSkinning";

test("eight native influences affect both animated geometry bounds and shader skinning", () => {
  const geometry = new BufferGeometry();
  geometry.setAttribute(
    "position",
    new Float32BufferAttribute([0, 0, 0, 1, 0, 0, 0, 1, 0], 3),
  );
  geometry.setAttribute(
    "skinIndex",
    new Uint16BufferAttribute([0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3], 4),
  );
  // Simulate GLTFLoader normalizing the first set before the adapter runs.
  geometry.setAttribute(
    "skinWeight",
    new Float32BufferAttribute(Array(12).fill(0.25), 4),
  );
  geometry.setAttribute(
    "joints_1",
    new Uint16BufferAttribute([4, 5, 6, 7, 4, 5, 6, 7, 4, 5, 6, 7], 4),
  );
  geometry.setAttribute(
    "weights_1",
    new Float32BufferAttribute(Array(12).fill(0.15), 4),
  );
  const bones = Array.from({ length: 8 }, (_, i) => {
    const bone = new Bone();
    bone.position.x = i;
    return bone;
  });
  const material = new MeshStandardMaterial();
  const mesh = new SkinnedMesh(geometry, material),
    root = new Group();
  root.add(mesh, ...bones);
  root.updateMatrixWorld(true);
  mesh.bind(
    new Skeleton(
      bones,
      bones.map(() => new Matrix4()),
    ),
    new Matrix4(),
  );
  nativeSkinning(root);
  nativeSkinning(root); // Rebinding the same geometry must not rescale weights twice.
  const point = mesh.applyBoneTransform(0, new Vector3());
  assert.ok(
    Math.abs(point.x - 3.9) < 1e-5,
    `all eight influences should produce x=3.9, got ${point.x}`,
  );
  bones[7].position.x += 2;
  root.updateMatrixWorld(true);
  assert.ok(
    Math.abs(mesh.applyBoneTransform(0, new Vector3()).x - 4.2) < 1e-5,
    "second influence set must follow live animation",
  );
  assert.ok(
    Math.abs(
      (geometry.getAttribute("skinWeight") as BufferAttribute).getX(0) - 0.1,
    ) < 1e-6,
  );
  const shader = {
    vertexShader:
      "#include <skinning_pars_vertex>\n#include <skinbase_vertex>\n#include <skinnormal_vertex>\n#include <skinning_vertex>",
    uniforms: {},
  } as Parameters<Material["onBeforeCompile"]>[0];
  material.onBeforeCompile(shader, {} as WebGLRenderer);
  assert.match(shader.vertexShader, /attribute vec4 joints_1/);
  assert.match(shader.vertexShader, /weights_1.w \* boneMatD/);
  assert.match(shader.vertexShader, /boneMatD \* weights_1.w/);
  geometry.dispose();
  material.dispose();
  mesh.skeleton.dispose();
});
