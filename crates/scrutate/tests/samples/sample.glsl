#version 330 core
#include "common.glsl"
// GLSL sample
struct Light { vec3 pos; };
// Lambert term.
float lambert(vec3 n, vec3 l) { return max(dot(n, l), 0.0); }
void main() { gl_FragColor = vec4(lambert(vec3(1), vec3(1))); }
