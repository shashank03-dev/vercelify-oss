// GLSL ES 3.00 ports of the AeroShards WGSL shaders, for browsers without
// WebGPU (see webgl.js). Each one follows its WGSL original line by line.
// WebGL counts framebuffer rows from the bottom and WebGPU from the top, so
// shaders that work in pixels convert with `view.y - gl_FragCoord.y`; the rest
// read their inputs at gl_FragCoord / target size, which stays consistent.

const HEADER = `#version 300 es
precision highp float;
precision highp int;
precision highp sampler2D;
`;

const FRAME = `
struct Frame {
  vec4 view;
  vec4 eye;
  vec4 forward;
  vec4 right;
  vec4 up;
  vec4 shard;
  vec4 accent;
  vec4 marker;
  vec4 surface;
};
uniform Frame scene;
`;

// Storage buffers become RGBA32F textures, one vec4 per texel, row by row.
const fetcher = name => `
uniform sampler2D ${name};
vec4 ${name}At(int i) {
  int width = textureSize(${name}, 0).x;
  return texelFetch(${name}, ivec2(i % width, i / width), 0);
}
`;

const SHARD_SHARED = `${HEADER}${FRAME}
struct Layer {
  vec4 view;
  vec4 look;
};
uniform Layer layerInfo;
`;

export const SHARD_VERTEX = `${SHARD_SHARED}${fetcher('shards')}
uniform int uFirstInstance;

flat out vec4 vCenter;
flat out vec4 vAxis;
flat out vec4 vNormal;
flat out vec4 vP01;
flat out vec4 vP23;
flat out vec4 vP4;
flat out vec4 vTopA;
flat out vec4 vTopB;
flat out vec4 vTopC;
flat out vec4 vBottomA;
flat out vec4 vBottomB;
flat out vec4 vBottomC;

vec3 project(vec3 world) {
  vec3 offset = world - scene.eye.xyz;
  float depth = max(dot(offset, scene.forward.xyz), 0.05);
  float x = dot(offset, scene.right.xyz);
  float y = dot(offset, scene.up.xyz);
  return vec3(layerInfo.view.x * 0.5 + x * layerInfo.view.z / depth, layerInfo.view.y * 0.5 - y * layerInfo.view.z / depth, depth);
}

void main() {
  int index = (gl_InstanceID + uFirstInstance) * 6;
  vec4 a = shardsAt(index);
  vec4 b = shardsAt(index + 1);
  vec4 c = shardsAt(index + 2);
  vec4 d = shardsAt(index + 3);
  vec4 e = shardsAt(index + 4);
  vec4 f = shardsAt(index + 5);
  vec3 center = a.xyz;
  float size = a.w;
  vec3 axis = normalize(b.xyz);
  vec3 normal = normalize(c.xyz);
  vec3 side = normalize(cross(normal, axis));

  vec3 middle = project(center);
  if (middle.z < 0.35 || size <= 0.0) {
    gl_Position = vec4(2.0, 2.0, 0.5, 1.0);
    return;
  }

  int count = int(f.z);
  vec2 points[5] = vec2[5](d.xy, d.zw, e.xy, e.zw, f.xy);
  vec2 low = vec2(1e9);
  vec2 high = vec2(-1e9);
  vec2 tops[5];
  vec2 bottoms[5];
  for (int i = 0; i < 5; i += 1) {
    vec2 corner = points[min(i, count - 1)];
    vec3 onPlate = center + (axis * corner.x + side * corner.y) * size;
    vec3 top = project(onPlate + normal * b.w * size);
    vec3 bottom = project(onPlate - normal * b.w * size);
    tops[i] = top.xy;
    bottoms[i] = bottom.xy;
    low = min(low, min(top.xy, bottom.xy));
    high = max(high, max(top.xy, bottom.xy));
  }
  low -= vec2(2.0);
  high += vec2(2.0);
  vec2 corners[6] = vec2[6](vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0), vec2(0.0, 1.0), vec2(1.0, 0.0), vec2(1.0, 1.0));
  vec2 pixel = mix(low, high, corners[gl_VertexID % 6]);
  gl_Position = vec4(pixel.x / layerInfo.view.x * 2.0 - 1.0, 1.0 - pixel.y / layerInfo.view.y * 2.0, 0.5, 1.0);
  vCenter = a;
  vAxis = vec4(axis, b.w);
  vNormal = vec4(normal, c.w);
  vP01 = d;
  vP23 = e;
  vP4 = f;
  vTopA = vec4(tops[0], tops[1]);
  vTopB = vec4(tops[2], tops[3]);
  vTopC = vec4(tops[4], 0.0, 0.0);
  vBottomA = vec4(bottoms[0], bottoms[1]);
  vBottomB = vec4(bottoms[2], bottoms[3]);
  vBottomC = vec4(bottoms[4], 0.0, 0.0);
}
`;

const SHARD_FRAGMENT = `${SHARD_SHARED}
flat in vec4 vCenter;
flat in vec4 vAxis;
flat in vec4 vNormal;
flat in vec4 vP01;
flat in vec4 vP23;
flat in vec4 vP4;
flat in vec4 vTopA;
flat in vec4 vTopB;
flat in vec4 vTopC;
flat in vec4 vBottomA;
flat in vec4 vBottomB;
flat in vec4 vBottomC;

out vec4 fragColor;

float cross2(vec2 a, vec2 b) {
  return a.x * b.y - a.y * b.x;
}

float outlineCoverage(vec2 p, vec2 points[5], int count) {
  float area = 0.0;
  for (int i = 0; i < 5; i += 1) {
    if (i < count) {
      area += cross2(points[i], points[(i + 1) % count]);
    }
  }
  float orient = area > 0.0 ? 1.0 : -1.0;
  float distance = -1e9;
  for (int i = 0; i < 5; i += 1) {
    if (i < count) {
      vec2 a = points[i];
      vec2 edge = points[(i + 1) % count] - a;
      float span = length(edge);
      if (span > 1e-4) {
        vec2 n = vec2(edge.y, -edge.x) * (orient / span);
        distance = max(distance, dot(p - a, n));
      }
    }
  }
  return clamp(0.5 - distance, 0.0, 1.0);
}

struct Prism {
  vec2 points[5];
  int count;
  float halfDepth;
};

struct Hit {
  float tNear;
  float tFar;
  vec3 normal;
};

vec4 plane(Prism prism, int k) {
  if (k == 0) { return vec4(0.0, 0.0, 1.0, prism.halfDepth); }
  if (k == 1) { return vec4(0.0, 0.0, -1.0, prism.halfDepth); }
  vec2 a = prism.points[k - 2];
  vec2 b = prism.points[(k - 1) % prism.count];
  vec2 edge = b - a;
  vec2 n = normalize(vec2(edge.y, -edge.x));
  return vec4(n, 0.0, dot(n, a));
}

Hit trace(Prism prism, vec3 ro, vec3 rd) {
  Hit hit;
  hit.tNear = -1e9;
  hit.tFar = 1e9;
  hit.normal = vec3(0.0, 0.0, 1.0);
  for (int k = 0; k < 7; k += 1) {
    if (k < prism.count + 2) {
      vec4 p = plane(prism, k);
      float denom = dot(p.xyz, rd);
      float gap = p.w - dot(p.xyz, ro);
      if (abs(denom) < 1e-7) {
        if (gap < 0.0) { hit.tFar = -1e9; }
      } else {
        float t = gap / denom;
        if (denom < 0.0) {
          if (t > hit.tNear) {
            hit.tNear = t;
            hit.normal = p.xyz;
          }
        } else if (t < hit.tFar) {
          hit.tFar = t;
        }
      }
    }
  }
  return hit;
}

vec4 exitFrom(Prism prism, vec3 ro, vec3 rd) {
  float best = 1e9;
  vec3 normal = vec3(0.0, 0.0, -1.0);
  for (int k = 0; k < 7; k += 1) {
    if (k < prism.count + 2) {
      vec4 p = plane(prism, k);
      float denom = dot(p.xyz, rd);
      if (denom > 1e-6) {
        float t = (p.w - dot(p.xyz, ro)) / denom;
        if (t < best) {
          best = t;
          normal = p.xyz;
        }
      }
    }
  }
  return vec4(normal, max(best, 0.0));
}

float rectLight(vec3 r, vec3 dir, vec3 across, vec2 extent, float soft) {
  float facing = dot(r, dir);
  if (facing <= 0.0) { return 0.0; }
  vec3 up = cross(dir, across);
  float u = dot(r, across) / facing;
  float v = dot(r, up) / facing;
  float a = 1.0 - smoothstep(extent.x - soft, extent.x + soft, abs(u));
  float b = 1.0 - smoothstep(extent.y - soft, extent.y + soft, abs(v));
  return a * b;
}

#define KEY_DIR normalize(vec3(-0.28, 0.72, 0.64))
#define KEY_ACROSS normalize(vec3(0.92, 0.0, 0.4))
#define LEFT_DIR normalize(vec3(-0.86, 0.12, 0.5))
#define LEFT_ACROSS normalize(vec3(0.5, 0.0, 0.86))
#define RIM_DIR normalize(vec3(0.78, 0.22, -0.58))
#define RIM_ACROSS normalize(vec3(0.6, 0.0, 0.8))
#define TOP_DIR normalize(vec3(0.05, 0.94, -0.33))
#define TOP_ACROSS vec3(1.0, 0.0, 0.0)

float lobe(vec3 r, vec3 dir, float width) {
  return exp((dot(r, dir) - 1.0) / (width * width));
}

vec3 studio(vec3 r, float soft, vec3 accent, vec3 ink) {
  float sky = clamp(r.y * 0.5 + 0.5, 0.0, 1.0);
  vec3 col = mix(ink * 0.03, ink * 0.12 + accent * 0.03, smoothstep(0.25, 0.95, sky));
  col += mix(ink, vec3(1.0), 0.6) * lobe(r, KEY_DIR, 0.38) * 3.6;
  col += mix(accent, vec3(1.0), 0.2) * lobe(r, RIM_DIR, 0.34) * 3.4;
  col += vec3(1.0, 0.98, 1.0) * rectLight(r, LEFT_DIR, LEFT_ACROSS, vec2(0.12, 0.8), 0.08 + soft) * 3.2;
  col += vec3(1.0, 0.98, 1.0) * rectLight(r, TOP_DIR, TOP_ACROSS, vec2(0.9, 0.1), 0.08 + soft) * 2.6;
  return col;
}

vec3 studioLight(vec3 r, float soft, vec3 accent) {
  vec3 ground = vec3(0.2, 0.2, 0.23);
  vec3 skyTone = vec3(0.97, 0.97, 0.99);
  vec3 col = mix(ground, skyTone, smoothstep(-0.08 - soft, 0.16 + soft, r.y));
  col = mix(col, vec3(0.55, 0.55, 0.6), lobe(r, vec3(0.0, -1.0, 0.0), 0.5) * 0.4);
  col += vec3(0.4) * rectLight(r, KEY_DIR, KEY_ACROSS, vec2(0.46, 0.3), 0.07 + soft);
  col -= vec3(0.55) * rectLight(r, LEFT_DIR, LEFT_ACROSS, vec2(0.1, 0.85), 0.05 + soft);
  col -= vec3(0.45) * rectLight(r, RIM_DIR, RIM_ACROSS, vec2(0.12, 0.9), 0.05 + soft);
  return clamp(col, vec3(0.05), vec3(1.2));
}

float schlick(float cosine, float f0) {
  return f0 + (1.0 - f0) * pow(1.0 - clamp(cosine, 0.0, 1.0), 5.0);
}

vec3 rayFor(vec2 pixel) {
  float x = pixel.x - layerInfo.view.x * 0.5;
  float y = layerInfo.view.y * 0.5 - pixel.y;
  return normalize(scene.forward.xyz * layerInfo.view.z + scene.right.xyz * x + scene.up.xyz * y);
}

struct Surface {
  float coverage;
  vec3 color;
  float alpha;
  float depth;
};

Surface shade(vec2 pixel) {
  vec3 center = vCenter.xyz;
  float size = vCenter.w;
  vec3 axis = vAxis.xyz;
  vec3 plateNormal = vNormal.xyz;
  vec3 side = normalize(cross(plateNormal, axis));
  Prism prism;
  prism.points = vec2[5](vP01.xy, vP01.zw, vP23.xy, vP23.zw, vP4.xy);
  prism.count = int(vP4.z);
  prism.halfDepth = vAxis.w;

  vec3 eye = scene.eye.xyz;
  mat3 toLocal = mat3(axis.x, side.x, plateNormal.x, axis.y, side.y, plateNormal.y, axis.z, side.z, plateNormal.z);
  mat3 fromLocal = mat3(axis, side, plateNormal);
  vec3 origin = toLocal * (eye - center) / size;

  vec3 rd = rayFor(pixel);
  vec3 ray = toLocal * rd;
  float denom = ray.z;
  float flatT = -origin.z / (abs(denom) < 1e-4 ? 1e-4 : denom);
  vec2 flatPoint = origin.xy + ray.xy * flatT;
  float footprint = max(length(fwidth(flatPoint)) * 0.7, 1e-4);
  float softBase = clamp(length(fwidth(reflect(rd, plateNormal))) * 0.5, 0.0, 0.5);

  Hit primary = trace(prism, origin, ray);
  vec2 tops[5] = vec2[5](vTopA.xy, vTopA.zw, vTopB.xy, vTopB.zw, vTopC.xy);
  vec2 bottoms[5] = vec2[5](vBottomA.xy, vBottomA.zw, vBottomB.xy, vBottomB.zw, vBottomC.xy);
  float coverage = max(outlineCoverage(pixel, tops, prism.count), outlineCoverage(pixel, bottoms, prism.count));

  Surface surface;
  float facing = origin.z > 0.0 ? 1.0 : -1.0;
  float capT = (facing * prism.halfDepth - origin.z) / (abs(ray.z) < 1e-4 ? -1e-4 * facing : ray.z);
  bool missed = primary.tNear > primary.tFar;
  float hitT = missed ? capT : primary.tNear;
  vec3 hitNormal = missed ? vec3(0.0, 0.0, facing) : primary.normal;
  surface.coverage = hitT > 0.0 ? coverage : 0.0;
  surface.color = vec3(0.0);
  surface.alpha = 0.0;
  surface.depth = 1.0;
  if (surface.coverage <= 0.0) { return surface; }
  vec3 mainRay = ray;

  vec3 local = origin + mainRay * hitT;
  vec3 worldRay = fromLocal * mainRay;
  float viewDepth = max(hitT * size * dot(worldRay, scene.forward.xyz), 0.05);
  surface.depth = clamp(1.0 - 0.3 / viewDepth, 0.0, 1.0);

  float radius = max(prism.halfDepth * 1.15, footprint * 2.5);
  vec3 n = hitNormal;
  float bevel = 0.0;
  if (abs(n.z) > 0.5) {
    float gap = 1e9;
    for (int k = 2; k < 7; k += 1) {
      if (k < prism.count + 2) {
        vec4 p = plane(prism, k);
        gap = min(gap, p.w - dot(p.xy, local.xy));
      }
    }
    vec2 toward = vec2(0.0);
    for (int k = 2; k < 7; k += 1) {
      if (k < prism.count + 2) {
        vec4 p = plane(prism, k);
        float g = p.w - dot(p.xy, local.xy);
        toward += p.xy * exp(-(g - gap) / (radius * 0.35));
      }
    }
    toward = normalize(toward);
    bevel = clamp(1.0 - gap / radius, 0.0, 1.0);
    float angle = bevel * bevel * 0.7853982;
    n = normalize(n * cos(angle) + vec3(toward, 0.0) * sin(angle));
  } else {
    float gap = prism.halfDepth - abs(local.z);
    bevel = clamp(1.0 - gap / radius, 0.0, 1.0);
    float angle = bevel * bevel * 0.7853982;
    n = normalize(n * cos(angle) + vec3(0.0, 0.0, sign(local.z)) * sin(angle));
  }
  float bendRate = 1.8 / max(radius / footprint, 1.0);
  vec3 normal = normalize(fromLocal * n);
  vec3 view = -worldRay;
  vec3 reflected = reflect(worldRay, normal);
  float soft = softBase + bendRate * smoothstep(0.0, 0.25, bevel) * 1.2 + clamp(footprint * 0.6, 0.0, 0.5);
  float ndv = clamp(dot(normal, view), 0.0, 1.0);

  vec3 accent = scene.accent.rgb;
  vec3 base = scene.shard.rgb;
  vec3 world = center + fromLocal * local * size;
  float height = clamp(0.5 + world.y * 0.45, 0.0, 1.0);
  vec3 tone = mix(accent, base, height);
  vec3 body = mix(tone, mix(base, accent, 0.5), vNormal.w * 0.3);
  float mode = scene.surface.x;
  bool lightMode = scene.up.w > 0.5;
  float wrapLight = clamp(dot(normal, KEY_DIR) * 0.5 + 0.5, 0.0, 1.0);

  vec3 color = vec3(0.0);
  float alpha = 1.0;
  bool chrome = mode > 0.5;
  if (lightMode) {
    vec3 env = studioLight(reflected, soft, accent);
    if (chrome) {
      color = env * mix(body, vec3(1.0), 0.6);
    } else {
      float fresnel = schlick(ndv, 0.05);
      float diffuse = clamp(dot(normal, KEY_DIR) * 0.5 + 0.5, 0.0, 1.0);
      alpha = clamp(0.32 + fresnel * 0.5 + (1.0 - abs(n.z)) * 0.5, 0.0, 1.0);
      color = mix(body, vec3(1.0), 0.15) * (0.5 + 0.4 * diffuse) * alpha + (env - vec3(0.78)) * fresnel * 0.8;
    }
  } else if (chrome) {
    vec3 env = studio(reflected, soft, accent, body);
    color = env * mix(body, vec3(1.0), 0.55) * 0.9 + body * 0.02;
  } else {
    float fresnel = schlick(ndv, 0.04);
    vec3 env = studio(reflected, soft, accent, body);
    vec3 light = vec3(0.0);
    vec3 cursor = local;
    vec3 direction = toLocal * refract(worldRay, normal, 1.0 / 1.5);
    float travelled = 0.0;
    float carry = 1.0 - fresnel;
    float through = 0.0;
    for (int bounce = 0; bounce < 3; bounce += 1) {
      vec4 outlet = exitFrom(prism, cursor, direction);
      travelled += outlet.w;
      cursor = cursor + direction * outlet.w;
      vec3 outward = outlet.xyz;
      vec3 escaped = refract(direction, -outward, 1.5);
      if (dot(escaped, escaped) > 0.0) {
        float leave = schlick(dot(direction, outward), 0.04);
        vec3 worldOut = normalize(fromLocal * escaped);
        vec3 absorb = exp(-(vec3(1.0) - body) * travelled * size * 22.0);
        float straight = smoothstep(0.97, 0.999, dot(worldOut, worldRay));
        float energy = carry * (1.0 - leave);
        light += studio(worldOut, soft * 2.0 + 0.04, accent, body) * absorb * energy * (1.0 - straight) * 2.0;
        through += energy * straight * (absorb.r + absorb.g + absorb.b) / 3.0;
        carry *= leave;
      }
      direction = reflect(direction, outward);
    }
    float edge = max(pow(max(1.0 - abs(n.z), 0.0), 1.2), bevel * bevel * 0.85);
    vec3 glow = mix(mix(body, accent, 0.35), vec3(1.0), 0.28) * edge * (0.35 + 0.45 * wrapLight);
    color = env * fresnel * 0.85 + light * (1.0 - smoothstep(0.0, 0.6, bevel)) + body * 0.02 + glow;
    alpha = clamp(1.0 - through + edge * 0.25, 0.0, 1.0);
  }

  color *= lightMode ? 1.0 : 1.3;
  float haze = smoothstep(3.5, 12.0, viewDepth) * layerInfo.look.x;
  vec3 hazeColor = lightMode ? mix(base, vec3(1.0), 0.72) : mix(base, accent, 0.5) * 0.3;
  color = mix(color, hazeColor * alpha + color * 0.3, haze);
  float flash = vP4.w;
  color += mix(accent, vec3(1.0), 0.35) * flash * (lightMode ? 0.0 : 0.5) * max(alpha, 0.35);
  alpha = max(alpha, flash * 0.45 * (lightMode ? 0.0 : 1.0));
  surface.color = color;
  surface.alpha = alpha;
  return surface;
}

// WebGPU's pixel position, with rows counted from the top.
vec2 pixelDown() {
  return vec2(gl_FragCoord.x, layerInfo.view.y - gl_FragCoord.y);
}
`;

export const SHARD_SOLID = `${SHARD_FRAGMENT}
void main() {
  Surface surface = shade(pixelDown());
  if (surface.coverage < 0.999 || surface.alpha < 0.6) { discard; }
  fragColor = vec4(surface.color, surface.alpha);
  gl_FragDepth = surface.depth;
}
`;

export const SHARD_EDGE = `${SHARD_FRAGMENT}
void main() {
  Surface surface = shade(pixelDown());
  if (surface.coverage <= 0.001 || (surface.coverage >= 0.9996 && surface.alpha >= 0.62)) { discard; }
  fragColor = vec4(surface.color, surface.alpha) * surface.coverage;
  gl_FragDepth = surface.depth;
}
`;

export const MARKER_VERTEX = `${HEADER}${FRAME}${fetcher('markers')}
uniform int uFirstInstance;

out vec2 vLocal;
flat out vec4 vShape;
flat out vec4 vLight;

void main() {
  int instance = gl_InstanceID + uFirstInstance;
  vec4 a = markersAt(instance * 2);
  vec4 b = markersAt(instance * 2 + 1);
  vec3 offset = a.xyz - scene.eye.xyz;
  float depth = max(dot(offset, scene.forward.xyz), 0.05);
  vec2 center = vec2(
    scene.view.x * 0.5 + dot(offset, scene.right.xyz) * scene.view.z / depth,
    scene.view.y * 0.5 - dot(offset, scene.up.xyz) * scene.view.z / depth
  );
  float pixel = scene.marker.y;
  float size = scene.marker.z * clamp(sqrt(scene.marker.w / depth), 0.6, 1.3) * b.z;
  float blur = min(scene.forward.w * abs(1.0 - scene.eye.w / depth), scene.right.w) * 0.18;
  float frame = b.y;
  float reach = (frame > 0.001 ? size * 2.6 : size) + blur + 1.5 * pixel;
  vec2 corners[6] = vec2[6](vec2(-1.0, -1.0), vec2(1.0, -1.0), vec2(-1.0, 1.0), vec2(-1.0, 1.0), vec2(1.0, -1.0), vec2(1.0, 1.0));
  vec2 corner = corners[gl_VertexID % 6] * reach;
  if (depth < 0.35 || a.w <= 0.002) {
    gl_Position = vec4(2.0, 2.0, 0.5, 1.0);
    vLocal = vec2(0.0);
    return;
  }
  vec2 at = center + corner;
  gl_Position = vec4(at.x / scene.view.x * 2.0 - 1.0, 1.0 - at.y / scene.view.y * 2.0, 0.5, 1.0);
  vLocal = corner;
  vShape = vec4(size, blur, frame, pixel);
  vLight = vec4(b.x, a.w, 0.0, 0.0);
}
`;

export const MARKER_FRAGMENT = `${HEADER}${FRAME}
in vec2 vLocal;
flat in vec4 vShape;
flat in vec4 vLight;
out vec4 fragColor;

float bar(float value, float edge, float soft) {
  return clamp((edge - value) / soft + 0.5, 0.0, 1.0);
}

void main() {
  float size = vShape.x;
  float blur = vShape.y;
  float frame = vShape.z;
  float pixel = vShape.w;
  vec2 q = abs(vLocal);
  float soft = 1.0 + blur;
  float thick = 0.6 * pixel + blur * 0.5;
  float energy = (0.6 * pixel) / thick;
  float cover = 0.0;
  if (scene.marker.x < 0.5) {
    float across = bar(q.x, size, soft) * bar(q.y, thick, soft);
    float down = bar(q.y, size, soft) * bar(q.x, thick, soft);
    cover = max(across, down) * energy;
  } else {
    cover = bar(length(vLocal), size * 0.42 + blur * 0.3, soft) * mix(1.0, energy, 0.6);
  }
  if (frame > 0.001) {
    float edge = size * 2.3;
    float span = edge * 0.45;
    float line = 0.5 * pixel;
    float sides = bar(abs(q.x - edge), line, 1.0) * bar(edge - span - q.y, 0.0, 1.0) * bar(q.y - edge - line, 0.0, 1.0);
    float caps = bar(abs(q.y - edge), line, 1.0) * bar(edge - span - q.x, 0.0, 1.0) * bar(q.x - edge - line, 0.0, 1.0);
    cover = max(cover, max(sides, caps) * frame * 0.85);
  }
  float glow = vLight.x;
  float fade = vLight.y;
  vec3 tint = mix(scene.shard.rgb, scene.accent.rgb, 0.4);
  if (scene.up.w > 0.5) {
    float ink = (1.0 - exp(-glow * 1.6)) * 0.85 * cover * fade;
    fragColor = vec4(mix(tint, scene.accent.rgb, 0.5) * 0.55 * ink, ink);
    return;
  }
  float light = cover * glow * fade;
  vec3 color = (1.0 - exp(-mix(tint, vec3(1.0), 0.35) * light * 1.6)) * 1.4;
  fragColor = vec4(color, max(max(color.r, color.g), color.b) * 0.8);
}
`;

// Full-screen passes. uTargetSize is the size of the target being drawn.
const EFFECT = `${HEADER}
uniform vec2 uTargetSize;
out vec4 fragColor;
`;

export const LAYER_BLUR = `${EFFECT}
struct Spread { vec4 texel; };
uniform sampler2D source;
uniform Spread spreadInfo;

void main() {
  vec2 uv = gl_FragCoord.xy / uTargetSize;
  float sigma = max(spreadInfo.texel.z, 0.3);
  int reach = min(int(ceil(sigma * 2.6)), 20);
  vec4 total = textureLod(source, uv, 0.0);
  float weight = 1.0;
  for (int i = 1; i <= reach; i += 1) {
    float w = exp(-float(i * i) / (2.0 * sigma * sigma));
    vec2 o = spreadInfo.texel.xy * float(i);
    total += (textureLod(source, uv + o, 0.0) + textureLod(source, uv - o, 0.0)) * w;
    weight += 2.0 * w;
  }
  fragColor = total / weight;
}
`;

export const SHADOW = `${EFFECT}
struct Shade {
  vec4 info;
  vec4 more;
};
uniform sampler2D source;
uniform sampler2D depthMap;
uniform Shade shadeInfo;

ivec2 extent;

// A top-down pixel (as in the WGSL) to a WebGL texel, clamped to the image.
ivec2 texel(ivec2 p) {
  p = clamp(p, ivec2(0), extent - ivec2(1));
  return ivec2(p.x, extent.y - 1 - p.y);
}

float depthAt(ivec2 p) {
  return texelFetch(depthMap, texel(p), 0).r;
}

void main() {
  extent = textureSize(source, 0);
  vec2 pixel = vec2(gl_FragCoord.x, float(extent.y) - gl_FragCoord.y);
  ivec2 here = ivec2(pixel);
  vec4 color = texelFetch(source, texel(here), 0);
  if (color.a <= 0.002 || shadeInfo.info.z <= 0.0) {
    fragColor = color;
    return;
  }
  float depth = depthAt(here);
  float east = depthAt(here + ivec2(1, 0));
  float west = depthAt(here - ivec2(1, 0));
  float south = depthAt(here + ivec2(0, 1));
  float north = depthAt(here - ivec2(0, 1));
  float limit = shadeInfo.more.x * 0.5;
  float slopeX = clamp(abs(depth - west) < abs(east - depth) ? depth - west : east - depth, -limit, limit);
  float slopeY = clamp(abs(depth - north) < abs(south - depth) ? depth - north : south - depth, -limit, limit);
  float occlusion = 0.0;
  for (int i = 1; i <= 12; i += 1) {
    vec2 offset = -shadeInfo.info.xy * float(i);
    float other = depthAt(ivec2(pixel + offset));
    float expected = depth + slopeX * offset.x + slopeY * offset.y;
    float closer = smoothstep(shadeInfo.more.x * 0.6, shadeInfo.more.x * 2.4, expected - other);
    float reach = 1.0 - float(i - 1) / 12.0;
    occlusion = max(occlusion, closer * reach * reach);
  }
  fragColor = vec4(color.rgb * (1.0 - occlusion * shadeInfo.info.z), color.a);
}
`;

export const COMPOSITE = `${EFFECT}
struct Blend { vec4 info; };
uniform sampler2D source;
uniform Blend mixInfo;

void main() {
  fragColor = textureLod(source, gl_FragCoord.xy / uTargetSize, 0.0) * mixInfo.info.x;
}
`;

const BLOOM = `${EFFECT}
struct Bloom { vec4 texel; };
uniform sampler2D source;
uniform Bloom bloom;
`;

export const PREFILTER = `${BLOOM}
vec3 bright(vec2 uv) {
  vec3 color = textureLod(source, uv, 0.0).rgb;
  float peak = max(max(color.r, color.g), color.b);
  return color * smoothstep(bloom.texel.z, bloom.texel.z + bloom.texel.w, peak);
}

void main() {
  vec2 uv = gl_FragCoord.xy / uTargetSize;
  vec2 o = bloom.texel.xy;
  vec3 total = bright(uv + vec2(o.x, o.y)) + bright(uv + vec2(-o.x, o.y)) + bright(uv + vec2(o.x, -o.y)) + bright(uv - o);
  fragColor = vec4(total * 0.25, 0.0);
}
`;

export const DOWNSAMPLE = `${BLOOM}
void main() {
  vec2 uv = gl_FragCoord.xy / uTargetSize;
  vec2 o = bloom.texel.xy;
  vec4 color = textureLod(source, uv, 0.0) * 0.25;
  color += textureLod(source, uv + vec2(o.x, o.y), 0.0) * 0.1875;
  color += textureLod(source, uv + vec2(-o.x, o.y), 0.0) * 0.1875;
  color += textureLod(source, uv + vec2(o.x, -o.y), 0.0) * 0.1875;
  color += textureLod(source, uv - o, 0.0) * 0.1875;
  fragColor = color;
}
`;

export const BLUR = `${BLOOM}
void main() {
  vec2 uv = gl_FragCoord.xy / uTargetSize;
  vec2 delta = bloom.texel.xy;
  vec4 color = textureLod(source, uv, 0.0) * 0.181862;
  color += (textureLod(source, uv + delta * 1.423135, 0.0) + textureLod(source, uv - delta * 1.423135, 0.0)) * 0.284316;
  color += (textureLod(source, uv + delta * 3.326702, 0.0) + textureLod(source, uv - delta * 3.326702, 0.0)) * 0.106598;
  color += (textureLod(source, uv + delta * 5.242989, 0.0) + textureLod(source, uv - delta * 5.242989, 0.0)) * 0.018156;
  fragColor = color;
}
`;

export const FINISH = `${EFFECT}
struct Finish {
  vec4 info;
  vec4 paper;
  vec4 mode;
};
uniform sampler2D sceneTexture;
uniform sampler2D bloom0;
uniform sampler2D bloom1;
uniform sampler2D bloom2;
uniform sampler2D bloom3;
uniform Finish finish;

float hash12(vec2 value) {
  vec2 p = fract(value * vec2(0.1031, 0.1030));
  vec2 mixed = p + dot(p, p.yx + 33.33);
  return fract((mixed.x + mixed.y) * mixed.x);
}

float smootherstep(float x) {
  float t = clamp(x, 0.0, 1.0);
  return t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
}

void main() {
  vec2 pixel = gl_FragCoord.xy;
  vec2 uv = pixel / uTargetSize;
  vec4 sceneColor = texelFetch(sceneTexture, ivec2(pixel), 0);
  float alpha = clamp(sceneColor.a, 0.0, 1.0);
  float noise = hash12(floor(pixel) + vec2(finish.info.y, finish.info.y * 1.618)) - 0.5;
  float grain = 1.0 + noise * finish.info.x;
  vec3 rgb = vec3(0.0);
  float a = alpha;
  if (finish.mode.x > 0.5) {
    rgb = clamp(sceneColor.rgb, vec3(0.0), vec3(alpha)) * grain;
  } else {
    vec3 halo = textureLod(bloom0, uv, 0.0).rgb * 0.4;
    halo += textureLod(bloom1, uv, 0.0).rgb * 0.3;
    halo += textureLod(bloom2, uv, 0.0).rgb * 0.2;
    halo += textureLod(bloom3, uv, 0.0).rgb * 0.1;
    halo = halo * finish.info.z;
    rgb = (1.0 - exp(-max(sceneColor.rgb, vec3(0.0)) * finish.info.w)) * grain + halo / (1.0 + halo);
    a = max(alpha, max(max(rgb.r, rgb.g), rgb.b));
  }
  float vignette = finish.mode.y;
  if (vignette > 0.0) {
    vec2 size = vec2(textureSize(sceneTexture, 0));
    float reach = max(vignette * 0.5 * min(size.x, size.y), 1.0);
    vec2 fromEdge = min(pixel, size - pixel);
    float mask = smootherstep(fromEdge.x / reach) * smootherstep(fromEdge.y / reach);
    rgb *= mask;
    a *= mask;
  }
  rgb = clamp(rgb, vec3(0.0), vec3(max(a, 0.0)));
  if (finish.paper.a > 0.5) {
    fragColor = vec4(finish.paper.rgb * (1.0 - a) + rgb, 1.0);
    return;
  }
  fragColor = vec4(rgb, a);
}
`;
