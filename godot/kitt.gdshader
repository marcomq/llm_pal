shader_type canvas_item;
// KITT's voice display: three columns of backlit red segments, lit from the middle up and down.
// Brightness per segment comes from the script (row 0 = middle), which also lets the bulbs glow after.

uniform float center[8];
uniform float side[6];
uniform float aspect = 0.5625;
uniform float time_s = 0.0;

const float PITCH = 0.036;
const float SEG_H = 0.023;
const float COL_W = 0.095;
const float COL_GAP = 0.135;

float rbox(vec2 p, vec2 half_size, float r) {
	vec2 d = abs(p) - half_size + r;
	return length(max(d, 0.0)) + min(max(d.x, d.y), 0.0) - r;
}

float hash(vec2 p) {
	p = fract(p * vec2(123.34, 456.21));
	p += dot(p, p + 45.32);
	return fract(p.x * p.y);
}

float brightness(bool is_side, int row) {
	if (is_side) {
		return row < 6 ? side[row] : 0.0;
	}
	return row < 8 ? center[row] : 0.0;
}

// Incandescent: deep red when dim, warmer and brighter when fully on.
vec3 bulb(float b) {
	return mix(vec3(0.85, 0.03, 0.0), vec3(1.0, 0.16, 0.04), b * b) * b * 1.35;
}

void fragment() {
	vec2 p = (UV - 0.5) * vec2(aspect, 1.0);
	vec3 col = vec3(0.034, 0.031, 0.029) * (1.0 - 0.5 * length(p));

	float bezel = rbox(p, vec2(COL_GAP * 1.5, 0.3), 0.02);
	col = mix(col, vec3(0.006), 1.0 - smoothstep(-0.002, 0.004, bezel));
	col += vec3(0.05) * exp(-abs(bezel) * 600.0) * step(0.0, -p.y);

	vec3 light = vec3(0.0);
	for (int c = -1; c <= 1; c++) {
		bool is_side = c != 0;
		float rmax = is_side ? 5.0 : 7.0;
		float cx = float(c) * COL_GAP;
		float near = clamp(round(p.y / PITCH), -rmax, rmax);
		for (int k = -1; k <= 1; k++) {
			float r = near + float(k);
			if (abs(r) > rmax) {
				continue;
			}
			float d = rbox(p - vec2(cx, r * PITCH), vec2(COL_W, SEG_H) * 0.5, 0.004);
			float inside = 1.0 - smoothstep(-0.003, 0.003, d);
			float hot = 0.75 + 0.5 * (1.0 - smoothstep(-0.012, 0.0, d));
			vec3 lamp = bulb(brightness(is_side, int(abs(r))));
			col = mix(col, vec3(0.09, 0.012, 0.008), inside * 0.8);
			light += lamp * inside * hot + lamp * 0.3 * exp(-max(d, 0.0) * 80.0);
		}
	}
	col += light;

	vec2 v = UV - 0.5;
	col *= 1.0 - 0.6 * dot(v, v);
	col += (hash(UV * 997.0 + fract(time_s * 7.0)) - 0.5) * 0.02;
	COLOR = vec4(col, 1.0);
}
