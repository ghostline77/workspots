import {
  Matrix4,
  Material,
  Object3D,
  ShaderChunk,
  SkinnedMesh,
  Vector3,
  Vector4,
} from "three";
const preparedMaterials = new WeakSet<Material>();

/** Preserve both native influence sets, including browser bounds and picking. */
export function nativeSkinning(root: Object3D): void {
  const geometries = new Set();
  root.traverse((object) => {
    if (!(object instanceof SkinnedMesh)) return;
    const geometry = object.geometry;
    const extraJoints = geometry.getAttribute("joints_1");
    const extraWeights = geometry.getAttribute("weights_1");
    if (!extraJoints || !extraWeights) return;
    const primary = geometry.getAttribute("skinWeight");
    // GLTFLoader normalizes WEIGHTS_0 alone. Restore its share of all eight weights.
    if (!geometries.has(geometry) && !geometry.userData["ghostline_eight_weights_restored"]) {
      for (let i = 0; i < primary.count; i++) {
        const remaining = Math.max(
          0,
          1 -
            extraWeights.getX(i) -
            extraWeights.getY(i) -
            extraWeights.getZ(i) -
            extraWeights.getW(i),
        );
        primary.setXYZW(
          i,
          primary.getX(i) * remaining,
          primary.getY(i) * remaining,
          primary.getZ(i) * remaining,
          primary.getW(i) * remaining,
        );
      }
      primary.needsUpdate = true;
      geometries.add(geometry);
      geometry.userData["ghostline_eight_weights_restored"] = true;
    }
    const source = new Vector4(),
      sum = new Vector4(),
      transformed = new Vector4(),
      matrix = new Matrix4();
    const transform = (
      index: number,
      target: Vector3 | Vector4,
    ): Vector3 | Vector4 => {
      source
        .set(
          target.x,
          target.y,
          target.z,
          target instanceof Vector4 ? target.w : 1,
        )
        .applyMatrix4(object.bindMatrix);
      sum.set(0, 0, 0, 0);
      for (const [joints, weights] of [
        [geometry.getAttribute("skinIndex"), primary],
        [extraJoints, extraWeights],
      ]) {
        for (let j = 0; j < 4; j++) {
          const weight = weights.getComponent(index, j);
          if (weight === 0) continue;
          const bone = joints.getComponent(index, j);
          matrix.multiplyMatrices(
            object.skeleton.bones[bone].matrixWorld,
            object.skeleton.boneInverses[bone],
          );
          sum.addScaledVector(
            transformed.copy(source).applyMatrix4(matrix),
            weight,
          );
        }
      }
      if (target instanceof Vector4) target.set(sum.x, sum.y, sum.z, source.w);
      else target.set(sum.x, sum.y, sum.z);
      return target.applyMatrix4(object.bindMatrixInverse);
    };
    object.applyBoneTransform = transform as typeof object.applyBoneTransform;
    object.computeBoundingBox();
    object.computeBoundingSphere();
    object.frustumCulled = false;
    for (const material of (Array.isArray(object.material)
      ? object.material
      : [object.material]) as Material[]) {
      if (preparedMaterials.has(material)) continue;
      preparedMaterials.add(material);
      const original = material.onBeforeCompile;
      material.onBeforeCompile = (shader, renderer) => {
        original.call(material, shader, renderer);
        shader.vertexShader = shader.vertexShader
          .replace(
            "#include <skinning_pars_vertex>",
            "#include <skinning_pars_vertex>\n#ifdef USE_SKINNING\nattribute vec4 joints_1;\nattribute vec4 weights_1;\n#endif",
          )
          .replace(
            "#include <skinbase_vertex>",
            ShaderChunk.skinbase_vertex.replace(
              "#endif",
              "mat4 boneMatA = getBoneMatrix(joints_1.x);\nmat4 boneMatB = getBoneMatrix(joints_1.y);\nmat4 boneMatC = getBoneMatrix(joints_1.z);\nmat4 boneMatD = getBoneMatrix(joints_1.w);\n#endif",
            ),
          )
          .replace(
            "#include <skinnormal_vertex>",
            ShaderChunk.skinnormal_vertex.replace(
              "skinMatrix = bindMatrixInverse",
              "skinMatrix += weights_1.x * boneMatA + weights_1.y * boneMatB + weights_1.z * boneMatC + weights_1.w * boneMatD;\nskinMatrix = bindMatrixInverse",
            ),
          )
          .replace(
            "#include <skinning_vertex>",
            ShaderChunk.skinning_vertex.replace(
              "transformed = ( bindMatrixInverse",
              "skinned += (boneMatA * weights_1.x + boneMatB * weights_1.y + boneMatC * weights_1.z + boneMatD * weights_1.w) * skinVertex;\ntransformed = ( bindMatrixInverse",
            ),
          );
      };
      material.customProgramCacheKey = () =>
        "ghostline-native-eight-influences-v1";
      material.needsUpdate = true;
    }
  });
}
