// WebGL2 version of the part of vgpu that AeroShards uses (init, surface,
// target, sampler, uniforms, storage, draw, effect, frame), so the shards
// render in browsers without WebGPU. Shaders come from glsl.js, looked up by
// their WGSL source through the `sources` map passed to init.
//
// Differences from WebGPU, handled here:
// - storage buffers are RGBA32F textures read with texelFetch;
// - there is no firstInstance, so draws pass it as the uFirstInstance uniform;
// - rgba16float targets need EXT_color_buffer_float (or the half-float one);
//   without either they fall back to RGBA8, which clips highlights a little.

const STORAGE_WIDTH = 1024;

const FULLSCREEN = `#version 300 es
void main() {
  vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
  gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
`;

const compileShader = (gl, type, source) => {
  const shader = gl.createShader(type);
  gl.shaderSource(shader, source);
  gl.compileShader(shader);
  if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS) && !gl.isContextLost()) {
    const log = gl.getShaderInfoLog(shader);
    gl.deleteShader(shader);
    throw new Error(`AeroShards: shader did not compile: ${log}`);
  }
  return shader;
};

const linkProgram = (gl, vertexSource, fragmentSource) => {
  const vertex = compileShader(gl, gl.VERTEX_SHADER, vertexSource);
  const fragment = compileShader(gl, gl.FRAGMENT_SHADER, fragmentSource);
  const program = gl.createProgram();
  gl.attachShader(program, vertex);
  gl.attachShader(program, fragment);
  gl.linkProgram(program);
  gl.deleteShader(vertex);
  gl.deleteShader(fragment);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS) && !gl.isContextLost()) {
    const log = gl.getProgramInfoLog(program);
    gl.deleteProgram(program);
    throw new Error(`AeroShards: program did not link: ${log}`);
  }
  return program;
};

/** Picks the best colour format the GPU can render into and blend. */
const pickColorFormat = gl => {
  const half = !!gl.getExtension('EXT_color_buffer_float') || !!gl.getExtension('EXT_color_buffer_half_float');
  if (!half) return { internal: gl.RGBA8, type: gl.UNSIGNED_BYTE };
  const texture = gl.createTexture();
  gl.bindTexture(gl.TEXTURE_2D, texture);
  gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA16F, 4, 4, 0, gl.RGBA, gl.HALF_FLOAT, null);
  const fbo = gl.createFramebuffer();
  gl.bindFramebuffer(gl.FRAMEBUFFER, fbo);
  gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, texture, 0);
  const ok = gl.checkFramebufferStatus(gl.FRAMEBUFFER) === gl.FRAMEBUFFER_COMPLETE;
  gl.bindFramebuffer(gl.FRAMEBUFFER, null);
  gl.deleteFramebuffer(fbo);
  gl.deleteTexture(texture);
  return ok ? { internal: gl.RGBA16F, type: gl.HALF_FLOAT } : { internal: gl.RGBA8, type: gl.UNSIGNED_BYTE };
};

export async function init({ canvas, sources, powerPreference = 'high-performance' }) {
  const gl = canvas.getContext('webgl2', {
    alpha: true,
    premultipliedAlpha: true,
    antialias: false,
    depth: false,
    stencil: false,
    powerPreference
  });
  if (!gl) throw new Error('WebGL2 is not available');
  const errorListeners = new Set();
  const disposers = new Set();
  const onLost = event => {
    event.preventDefault();
    const error = new Error('WebGL context lost');
    errorListeners.forEach(listener => listener(error));
  };
  canvas.addEventListener('webglcontextlost', onLost);
  const vao = gl.createVertexArray();
  return {
    kind: 'webgl',
    gl,
    canvas,
    sources,
    vao,
    color: pickColorFormat(gl),
    disposers,
    onError(listener) {
      errorListeners.add(listener);
      return () => errorListeners.delete(listener);
    },
    dispose() {
      canvas.removeEventListener('webglcontextlost', onLost);
      errorListeners.clear();
      if (!gl.isContextLost()) {
        disposers.forEach(dispose => dispose());
        gl.deleteVertexArray(vao);
      }
      disposers.clear();
    }
  };
}

export function surface(instance, canvas, { size }) {
  const output = {
    kind: 'surface',
    size: [1, 1],
    resize(next) {
      output.size = [next[0], next[1]];
      canvas.width = next[0];
      canvas.height = next[1];
    }
  };
  output.resize(size);
  return output;
}

export function sampler() {
  // Every colour target already samples linearly with clamped edges.
  return { kind: 'sampler' };
}

export function target(instance, { size, depth }) {
  const { gl, color, disposers } = instance;
  const fbo = gl.createFramebuffer();
  const texture = gl.createTexture();
  const depthHandle = depth ? { kind: 'texture', texture: gl.createTexture() } : undefined;
  const result = {
    kind: 'target',
    size: [1, 1],
    texture,
    fbo,
    depth: depthHandle,
    resize(next) {
      result.size = [Math.max(1, next[0]), Math.max(1, next[1])];
      const [w, h] = result.size;
      gl.bindTexture(gl.TEXTURE_2D, texture);
      gl.texImage2D(gl.TEXTURE_2D, 0, color.internal, w, h, 0, gl.RGBA, color.type, null);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
      gl.bindFramebuffer(gl.FRAMEBUFFER, fbo);
      gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, texture, 0);
      if (depthHandle) {
        gl.bindTexture(gl.TEXTURE_2D, depthHandle.texture);
        gl.texImage2D(gl.TEXTURE_2D, 0, gl.DEPTH_COMPONENT32F, w, h, 0, gl.DEPTH_COMPONENT, gl.FLOAT, null);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
        gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
        gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.DEPTH_ATTACHMENT, gl.TEXTURE_2D, depthHandle.texture, 0);
      }
      gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    }
  };
  result.resize(size);
  disposers.add(() => {
    gl.deleteFramebuffer(fbo);
    gl.deleteTexture(texture);
    if (depthHandle) gl.deleteTexture(depthHandle.texture);
  });
  return result;
}

export function uniforms(instance, initial) {
  const values = {};
  for (const [name, value] of Object.entries(initial)) values[name] = Float32Array.from(value);
  return {
    kind: 'uniforms',
    values,
    set(next) {
      for (const [name, value] of Object.entries(next)) {
        if (!values[name]) values[name] = new Float32Array(4);
        values[name].set(value);
      }
    }
  };
}

export function storage(instance, byteLength) {
  const { gl, disposers } = instance;
  const texels = Math.max(1, Math.ceil(byteLength / 16));
  const rows = Math.ceil(texels / STORAGE_WIDTH);
  const padded = new Float32Array(STORAGE_WIDTH * rows * 4);
  const texture = gl.createTexture();
  gl.bindTexture(gl.TEXTURE_2D, texture);
  gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA32F, STORAGE_WIDTH, rows, 0, gl.RGBA, gl.FLOAT, null);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
  disposers.add(() => gl.deleteTexture(texture));
  return {
    kind: 'texture',
    texture,
    write(data) {
      const used = Math.min(rows, Math.ceil(data.length / 4 / STORAGE_WIDTH));
      if (used === 0) return;
      padded.set(data.subarray(0, Math.min(data.length, padded.length)));
      gl.bindTexture(gl.TEXTURE_2D, texture);
      gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, STORAGE_WIDTH, used, gl.RGBA, gl.FLOAT, padded, 0);
    }
  };
}

/** A program plus its bindings; draw() and effect() both return one. */
const makeItem = (instance, { vertex, fragment, vertices, depth, blend, fullscreen }) => {
  const { gl, disposers } = instance;
  let program = null;
  const bindings = {};
  const locations = new Map();
  const locate = name => {
    if (!locations.has(name)) locations.set(name, gl.getUniformLocation(program, name));
    return locations.get(name);
  };
  const build = () => {
    if (!program) {
      program = linkProgram(gl, vertex, fragment);
      disposers.add(() => gl.deleteProgram(program));
    }
    return program;
  };
  return {
    fullscreen,
    vertices,
    depth,
    blend,
    set(next) {
      Object.assign(bindings, next);
    },
    compile() {
      return new Promise(resolve => {
        build();
        resolve();
      });
    },
    use(targetSize, firstInstance) {
      gl.useProgram(build());
      let unit = 0;
      for (const [name, value] of Object.entries(bindings)) {
        if (!value || value.kind === 'sampler') continue;
        if (value.kind === 'uniforms') {
          for (const [field, data] of Object.entries(value.values)) {
            const location = locate(`${name}.${field}`);
            if (location) gl.uniform4fv(location, data);
          }
          continue;
        }
        const location = locate(name);
        if (!location) continue;
        gl.activeTexture(gl.TEXTURE0 + unit);
        gl.bindTexture(gl.TEXTURE_2D, value.texture);
        gl.uniform1i(location, unit);
        unit += 1;
      }
      const size = locate('uTargetSize');
      if (size) gl.uniform2f(size, targetSize[0], targetSize[1]);
      const first = locate('uFirstInstance');
      if (first) gl.uniform1i(first, firstInstance);
    }
  };
};

const sourceFor = (instance, wgsl) => {
  const source = instance.sources.get(wgsl);
  if (!source) throw new Error('AeroShards: no GLSL version of a shader');
  return source;
};

export function draw(instance, { shader, entry, vertices = 6, depth = false, blend }) {
  const source = sourceFor(instance, shader);
  const fragment = typeof source.fragment === 'string' ? source.fragment : source.fragment[entry?.fragment];
  return makeItem(instance, { vertex: source.vertex, fragment, vertices, depth, blend, fullscreen: false });
}

export function effect(instance, shader, { set, blend } = {}) {
  const source = sourceFor(instance, shader);
  const item = makeItem(instance, {
    vertex: FULLSCREEN,
    fragment: source.fragment,
    vertices: 3,
    depth: false,
    blend,
    fullscreen: true
  });
  if (set) item.set(set);
  return item;
}

const applyState = (gl, item) => {
  if (item.depth) {
    gl.enable(gl.DEPTH_TEST);
    gl.depthFunc(gl.LESS);
    gl.depthMask(!!item.depth.write);
  } else {
    gl.disable(gl.DEPTH_TEST);
  }
  if (item.blend === 'premultiplied') {
    gl.enable(gl.BLEND);
    gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA);
  } else {
    gl.disable(gl.BLEND);
  }
};

export function frame(instance, render) {
  const { gl, vao } = instance;
  if (gl.isContextLost()) return;
  gl.bindVertexArray(vao);
  gl.disable(gl.CULL_FACE);
  render({
    pass({ target: into, clear, clearDepth }, record) {
      const size = into.size;
      gl.bindFramebuffer(gl.FRAMEBUFFER, into.kind === 'surface' ? null : into.fbo);
      gl.viewport(0, 0, size[0], size[1]);
      let mask = 0;
      if (Array.isArray(clear)) {
        gl.clearColor(clear[0], clear[1], clear[2], clear[3]);
        mask |= gl.COLOR_BUFFER_BIT;
      }
      if (clearDepth !== undefined && into.depth) {
        gl.depthMask(true);
        gl.clearDepth(clearDepth);
        mask |= gl.DEPTH_BUFFER_BIT;
      }
      if (mask) gl.clear(mask);
      record({
        draw(item, { instances = 1, firstInstance = 0 } = {}) {
          applyState(gl, item);
          item.use(size, firstInstance);
          if (item.fullscreen) gl.drawArrays(gl.TRIANGLES, 0, 3);
          else gl.drawArraysInstanced(gl.TRIANGLES, 0, item.vertices, instances);
        }
      });
    }
  });
  gl.bindVertexArray(null);
}
