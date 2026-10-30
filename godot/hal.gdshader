shader_type canvas_item;
// HAL 9000: the eye is an image (scripts/make-hal-panel.py); only the red light is drawn here, added on top so the
// reflections in the image stay visible.

uniform float level = 0.0;
uniform float glow = 0.4;
// Lens centre in UV and lens radius as a fraction of the width; must match make-hal-panel.py.
uniform vec2 lens_center = vec2(0.5, 0.46875);
uniform float lens_radius = 0.1278;
uniform float aspect = 0.5625;

void fragment() {
	vec3 col = texture(TEXTURE, UV).rgb;
	vec2 p = (UV - lens_center) * vec2(1.0, 1.0 / aspect);
	float r = length(p) / lens_radius;
	float i = clamp(glow + level * 0.9, 0.0, 1.3);

	float lens = 1.0 - smoothstep(0.97, 1.0, r);
	float core = mix(1.0, 0.65, smoothstep(0.0, 0.4, r)) * (1.0 - smoothstep(0.4, 1.0, r) * 0.8);
	vec3 light = vec3(1.0, 0.04, 0.03) * core * lens;
	light += vec3(1.0, 0.06, 0.04) * 0.4 * exp(-max(r - 1.0, 0.0) * 2.2) * (1.0 - lens);
	light += vec3(1.0, 0.85, 0.45) * exp(-r * r * 30.0) * 0.9;
	col += light * i;
	COLOR = vec4(col, 1.0);
}
