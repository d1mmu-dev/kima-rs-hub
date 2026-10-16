import * as THREE from "three";
import { emphasisScale, focusScale, starDiscRadius, type StarUniforms } from "./starShader";

/**
 * A ring a fixed number of screen pixels outside a star's drawn disc, at any zoom:
 * a camera-facing square whose shader draws one circle, resized every frame by fitRing.
 */
const VERT = /* glsl */ `
varying vec2 vUv;
void main() {
  vUv = uv * 2.0 - 1.0;
  gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0);
}`;

const FRAG = /* glsl */ `
uniform vec3 uColor;
uniform float uOpacity;
uniform float uRadius;
uniform float uWidth;
uniform float uAA;
varying vec2 vUv;
void main() {
  float d = length(vUv);
  float inner = uRadius - uWidth * 0.5;
  float outer = uRadius + uWidth * 0.5;
  float a = smoothstep(inner - uAA, inner, d) * (1.0 - smoothstep(outer, outer + uAA, d));
  if (a <= 0.0) discard;
  gl_FragColor = vec4(uColor, a * uOpacity);
}`;

export function createRingMaterial(color: string, opacity: number) {
    return new THREE.ShaderMaterial({
        uniforms: {
            uColor: { value: new THREE.Color(color) },
            uOpacity: { value: opacity },
            uRadius: { value: 0.9 },
            uWidth: { value: 0.05 },
            uAA: { value: 0.02 },
        },
        vertexShader: VERT,
        fragmentShader: FRAG,
        transparent: true,
        depthWrite: false,
        depthTest: false,
    });
}

/** The star a ring goes round: its world position, size and mood attributes, and where to read its live emphasis and mood preview. */
export interface RingStar {
    at: [number, number, number];
    size: number;
    mood: number;
    emphasis: THREE.BufferAttribute;
    index: number;
    stars: StarUniforms;
}

function scaleOf(star: RingStar) {
    return emphasisScale(star.emphasis.getX(star.index)) * focusScale(star.mood, star.stars);
}

const view = new THREE.Vector3();
const ndc = new THREE.Vector3();

function viewDepth(at: [number, number, number], camera: THREE.Camera) {
    return Math.max(-view.set(...at).applyMatrix4(camera.matrixWorldInverse).z, 0.001);
}

/** CSS-pixel radius of a ring `gap` pixels out from the star's disc. */
export function ringRadius(star: RingStar, camera: THREE.Camera, pixelRatio: number, gap: number) {
    return starDiscRadius(star.size, scaleOf(star), viewDepth(star.at, camera), pixelRatio) + gap;
}

/** Where a star lands on screen in CSS pixels, or null behind the camera. */
export function screenOf(at: [number, number, number], camera: THREE.Camera, width: number, height: number): [number, number] | null {
    ndc.set(...at).project(camera);
    if (!Number.isFinite(ndc.z) || ndc.z >= 1) return null;
    return [((ndc.x + 1) / 2) * width, ((1 - ndc.y) / 2) * height];
}

/** Sizes `mesh` (a 2×2 plane with a ring material) so its ring sits `gap` CSS pixels outside the disc, `width` wide, times `swell`; returns the radius. */
export function fitRing(
    mesh: THREE.Mesh,
    star: RingStar,
    camera: THREE.PerspectiveCamera,
    viewHeight: number,
    pixelRatio: number,
    gap: number,
    width: number,
    swell = 1,
) {
    const depth = viewDepth(star.at, camera);
    const radius = (starDiscRadius(star.size, scaleOf(star), depth, pixelRatio) + gap) * swell;
    const half = radius + width + 1.5;
    const worldPerPx = (2 * depth * Math.tan(THREE.MathUtils.degToRad(camera.fov) / 2)) / Math.max(viewHeight, 1);
    mesh.scale.setScalar(half * worldPerPx);
    const u = (mesh.material as THREE.ShaderMaterial).uniforms;
    u.uRadius.value = radius / half;
    u.uWidth.value = width / half;
    // One device pixel of edge softening, so a thin ring stays thin on any screen.
    u.uAA.value = 1 / (pixelRatio * half);
    return radius;
}
