use super::*;

fn px(c: &Context, x: i32, y: i32) -> [u8; 4] {
    let d = c.get_image_data(x, y, 1, 1);
    [d[0], d[1], d[2], d[3]]
}

#[test]
fn fills_rects_and_paths() {
    let mut c = Context::new(20, 20);
    c.state.fill = Paint::Solid(premul(Color { r: 255, g: 0, b: 0, a: 255 }));
    c.fill_rect(2.0, 2.0, 10.0, 10.0);
    assert_eq!(px(&c, 5, 5), [255, 0, 0, 255]);
    assert_eq!(px(&c, 15, 15), [0, 0, 0, 0]);
    // Half a pixel covered: half alpha.
    let mut c = Context::new(4, 4);
    c.fill_rect(0.0, 0.0, 1.5, 4.0);
    assert_eq!(px(&c, 1, 1)[3], 128);
}

#[test]
fn nonzero_and_evenodd() {
    let mut c = Context::new(30, 30);
    let m = IDENTITY;
    let mut p = Path::default();
    p.rect(&m, 0.0, 0.0, 30.0, 30.0);
    p.rect(&m, 10.0, 10.0, 10.0, 10.0);
    c.fill_path(&p, true);
    assert_eq!(px(&c, 15, 15)[3], 0);
    assert_eq!(px(&c, 5, 5)[3], 255);
    c.fill_path(&p, false);
    assert_eq!(px(&c, 15, 15)[3], 255);
}

#[test]
fn strokes_with_width_and_caps() {
    let mut c = Context::new(40, 40);
    c.state.line_width = 10.0;
    let mut p = Path::default();
    p.move_to(&IDENTITY, 10.0, 20.0);
    p.line_to(&IDENTITY, 30.0, 20.0);
    c.stroke_path(&p);
    assert_eq!(px(&c, 20, 16)[3], 255);
    assert_eq!(px(&c, 20, 26)[3], 0);
    assert_eq!(px(&c, 7, 20)[3], 0);
    c.state.cap = Cap::Square;
    c.stroke_path(&p);
    assert_eq!(px(&c, 7, 20)[3], 255);
}

#[test]
fn transforms_clip_and_composite() {
    let mut c = Context::new(20, 20);
    c.transform(&[2.0, 0.0, 0.0, 2.0, 0.0, 0.0]);
    c.fill_rect(0.0, 0.0, 5.0, 5.0);
    assert_eq!(px(&c, 9, 9)[3], 255);
    assert_eq!(px(&c, 11, 11)[3], 0);
    let mut c = Context::new(20, 20);
    let mut p = Path::default();
    p.rect(&IDENTITY, 0.0, 0.0, 10.0, 20.0);
    c.clip_path(&p, false);
    c.fill_rect(0.0, 0.0, 20.0, 20.0);
    assert_eq!(px(&c, 5, 5)[3], 255);
    assert_eq!(px(&c, 15, 5)[3], 0);
    // copy clears everything outside the shape (inside the clip).
    c.state.clip = None;
    c.fill_rect(0.0, 0.0, 20.0, 20.0);
    c.state.op = Op::Copy;
    c.state.fill = Paint::Solid(premul(Color { r: 0, g: 255, b: 0, a: 255 }));
    c.fill_rect(0.0, 0.0, 5.0, 5.0);
    assert_eq!(px(&c, 2, 2), [0, 255, 0, 255]);
    assert_eq!(px(&c, 10, 10)[3], 0);
    c.clear_rect(0.0, 0.0, 20.0, 20.0);
    assert_eq!(px(&c, 2, 2)[3], 0);
}

#[test]
fn gradients_and_images() {
    let mut c = Context::new(100, 10);
    let stops = vec![(0.0, premul(Color { r: 0, g: 0, b: 0, a: 255 })), (1.0, premul(Color { r: 255, g: 255, b: 255, a: 255 }))];
    c.state.fill = Paint::Gradient { g: Gradient::Linear { p0: (0.0, 0.0), p1: (100.0, 0.0) }, stops: Arc::new(stops) };
    c.fill_rect(0.0, 0.0, 100.0, 10.0);
    let mid = px(&c, 50, 5);
    assert!((mid[0] as i32 - 128).abs() <= 3, "{mid:?}");
    let img = Image::from_straight(2, 1, &[255, 0, 0, 255, 0, 0, 255, 255]);
    let mut c = Context::new(10, 10);
    c.state.smoothing = false;
    c.draw_image(&img, 0.0, 0.0, 2.0, 1.0, 0.0, 0.0, 10.0, 10.0);
    assert_eq!(px(&c, 2, 5), [255, 0, 0, 255]);
    assert_eq!(px(&c, 7, 5), [0, 0, 255, 255]);
}

#[test]
fn image_data_round_trips() {
    let mut c = Context::new(4, 4);
    c.put_image_data(&[10, 20, 30, 255, 200, 100, 50, 128], 2, 1, 1, 1, (0, 0, 2, 1));
    assert_eq!(px(&c, 1, 1), [10, 20, 30, 255]);
    let p = px(&c, 2, 1);
    assert!((p[0] as i32 - 200).abs() <= 2 && p[3] == 128, "{p:?}");
    assert!(c.png().starts_with(b"\x89PNG"));
}

#[test]
fn fonts_and_colors() {
    let f = parse_font("italic bold 14px Georgia, serif").unwrap();
    assert_eq!((f.size, f.weight, f.italic, f.family), (14.0, 700, true, Family::Serif));
    assert_eq!(parse_font("12pt monospace").unwrap().size, 16.0);
    assert!(parse_font("bogus").is_none());
    assert_eq!(serialize_color(Color { r: 255, g: 0, b: 0, a: 128 }), "rgba(255, 0, 0, 0.5)");
    assert_eq!(serialize_color(Color { r: 1, g: 2, b: 255, a: 255 }), "#0102ff");
}

#[test]
fn svg_paths_and_hit_tests() {
    let mut p = Path::default();
    p.svg(&IDENTITY, "M10 10 h 80 v 80 h -80 Z");
    assert!(path::contains(&p.polys(), (50.0, 50.0), false));
    assert!(!path::contains(&p.polys(), (5.0, 50.0), false));
    let mut a = Path::default();
    a.svg(&IDENTITY, "M 0 50 A 50 50 0 0 1 100 50");
    assert!(a.polys()[0].pts.len() > 8);
}


#[test]
fn shadows_of_shapes_off_the_canvas() {
    let mut c = Context::new(100, 50);
    c.state.fill = Paint::Solid([1.0, 0.0, 0.0, 1.0]);
    c.fill_rect(0.0, 0.0, 100.0, 50.0);
    c.state.shadow = [0.0, 1.0, 0.0, 1.0];
    c.state.shadow_offset = (0.0, 100.0);
    c.fill_rect(25.0, -100.0, 50.0, 25.0);
    let k = (12 * 100 + 50) * 4;
    assert_eq!(&c.pixels[k..k + 4], &[0, 255, 0, 255]);
    c.state.shadow_offset = (0.0, 50.0);
    c.state.shadow = [0.0, 0.0, 0.5, 0.5];
    c.fill_rect(0.0, -50.0, 100.0, 50.0);
    let k = (25 * 100 + 50) * 4;
    assert_eq!(&c.pixels[k..k + 4], &[128, 0, 128, 255]);
}
