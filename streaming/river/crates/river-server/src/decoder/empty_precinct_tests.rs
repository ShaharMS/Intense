use jpeg2k::{Image,DecodeParameters,DecodeArea};

#[test]
fn missing_precincts_are_zero_coefficients_before_dc_shift() {
    let fixtures:[(&[u8],u32);5]=[
        (include_bytes!("../../tests/fixtures/empty-precincts/quadrants8-mct0.j2k"),8),
        (include_bytes!("../../tests/fixtures/empty-precincts/quadrants8-mct1.j2k"),8),
        (include_bytes!("../../tests/fixtures/empty-precincts/quadrants16-mct0.j2k"),16),
        (include_bytes!("../../tests/fixtures/empty-precincts/quadrants16-mct1.j2k"),16),
        (include_bytes!("../../tests/fixtures/empty-precincts/quadrants8-mct1-dwt2.j2k"),8),
    ];
    for (bytes,bits) in fixtures {for threads in [1,4] {for roi in [false,true] {
        let mut params=DecodeParameters::new().threads(threads);
        if roi {params=params.decode_area(Some(DecodeArea::new(4,4,12,12)));}
        let image=Image::from_bytes_with(bytes,params).unwrap();let planes=image.components();assert_eq!(planes.len(),3);
        let (size,offset)=if roi {(8,4)}else{(16,0)};
        let palette=[[0;3],[1<<(bits-1);3],[(1<<bits)-1;3],if bits==8 {[30,100,200]}else{[4096,20000,60000]}];
        for (c,plane) in planes.iter().enumerate() {
            assert_eq!((plane.width(),plane.height(),plane.precision()),(size,size,bits));
            for y in 0..size {for x in 0..size {
                let expected=palette[(((y+offset)/8)*2+(x+offset)/8) as usize][c];
                assert_eq!(plane.data()[(y*size+x) as usize],expected,"bits={bits}, threads={threads}, roi={roi}, x={x},y={y},c={c}");
            }}
        }
    }}}
}

#[test]
fn fully_empty_zero_dwt_rgb_is_midgray() {
    let image=Image::from_bytes(include_bytes!("../../tests/fixtures/empty-precincts/view-r14.j2k")).unwrap();
    assert_eq!(image.components().len(),3);
    for plane in image.components(){assert_eq!((plane.width(),plane.height()),(1,1));assert_eq!(plane.data(),[128]);}
}
