//! Connected-source tile adapter with an optional client-defined pixel pyramid.
use super::*;

#[derive(serde::Deserialize)]
pub(super) struct TileQuery { image:String, #[serde(default)] driver:String, layers:Option<u32>, #[serde(rename="tileSize")] tile_size:Option<u32>, #[serde(rename="maxZoom")] max_zoom:Option<u8>, #[serde(rename="minZoom")] min_zoom:Option<u8>, #[serde(rename="tilePixelRatio")] pixel_ratio:Option<f64>, #[serde(default,alias="EDGE")] edge:Option<String> }

pub(super) async fn tile(State(state):State<AppState>,Path((z,x,y,file)):Path<(u8,u32,u32,String)>,Query(query):Query<TileQuery>)->Response {
    let geometry=match Geometry::parse(query.tile_size,query.max_zoom,query.min_zoom,query.pixel_ratio,z) {Ok(g)=>g,Err(e)=>return errors::reply(StatusCode::BAD_REQUEST,"invalid_tile_geometry",e.to_string(),serde_json::json!({"zoom":z}))};
    let mode=match edges::Mode::parse(query.edge.as_deref().unwrap_or("black")) {Ok(mode)=>mode,Err(error)=>return errors::reply(StatusCode::BAD_REQUEST,"invalid_edge_mode",error.to_string(),serde_json::json!({"allowed":["crop","black","transparent"]}))};
    let format=match file.strip_prefix("tile.").map(str::to_ascii_lowercase).as_deref() {
        Some("png")=>"image/png",Some("webp")=>"image/webp",Some("avif")=>"image/avif",Some("jxl")=>"image/jxl",
        _=>return errors::reply(StatusCode::BAD_REQUEST,"unsupported_tile_extension","Expected tile.png, tile.webp, tile.avif or tile.jxl.",serde_json::json!({"file":file})),
    };
    let asset=match resolve_image(&state,&query.image).await {Ok(asset)=>asset,Err(response)=>return response};
    let geometry=match geometry {
        Some(geometry)=>geometry,
        None=>match Geometry::default_for(asset.manifest.width,asset.manifest.height,z) {
            Ok(geometry)=>geometry,
            Err(error)=>return errors::reply(StatusCode::BAD_REQUEST,"invalid_tile_geometry",error.to_string(),serde_json::json!({"zoom":z})),
        },
    };
    let id=asset.manifest.asset_id.0;
    render_wmts(state,WmtsQuery {service:"WMTS".into(),request:"GetTile".into(),version:"1.0.0".into(),
        edge:mode.as_str().into(),layer:id.clone(),limit:None,cursor:None,style:"default".into(),format:format.into(),driver:query.driver,layers:query.layers,
        matrix_set:format!("RiverPixel-{id}"),matrix:z.to_string(),row:y.to_string(),col:x.to_string()},Some(geometry)).await
}

pub(super) async fn tile_info(State(state):State<AppState>,Path(image):Path<String>)->Response {
    let asset=match resolve_image(&state,&image).await {Ok(asset)=>asset,Err(response)=>return response};
    let (width,height)=(asset.manifest.width,asset.manifest.height);
    match default_max_zoom(width,height) {
        Ok(maxzoom)=>Json(serde_json::json!({"bounds":[0,height,width,0],"minzoom":0,"maxzoom":maxzoom})).into_response(),
        Err(error)=>errors::reply(StatusCode::UNPROCESSABLE_ENTITY,"unsupported_pixel_pyramid",error.to_string(),serde_json::json!({"width":width,"height":height})),
    }
}

// One256px root tile covers the image; increasing zoom approaches full scale.
// Codestream resolution levels only determine native decode reduction, not
// the external pixel pyramid. render_wmts clamps reduction before resampling.
fn default_max_zoom(width:u32,height:u32)->anyhow::Result<u8> {
    let extent=width.max(height);
    anyhow::ensure!(width>0 && height>0,"Image dimensions must be positive");
    let max=(u32::BITS-extent.div_ceil(256).saturating_sub(1).leading_zeros()) as u8;
    anyhow::ensure!(max<=23,"The256pixel compatibility pyramid supports dimensions up to2^31 pixels; explicit geometry can define a multi-root grid");
    Ok(max)
}

async fn resolve_image(state:&AppState,image:&str)->std::result::Result<RegisteredAsset,Response> {
    let mut id=image.to_owned();
    if !state.assets.read().await.contains_key(&AssetId(id.clone())) && remote_asset_key(&id).is_none() {
        let sources=state.s3_sources.read().await;
        let matches=sources.values().filter_map(|source|source_key(image,&source.bucket,&source.region,source.endpoint.as_deref())
            .filter(|key|source.prefix.is_empty() || key==&source.prefix || key.starts_with(&format!("{}/",source.prefix)))
            .map(|key|(source.id.clone(),key))).collect::<Vec<_>>();
        if matches.len()!=1 {
            return Err((StatusCode::BAD_REQUEST,Json(serde_json::json!({"code":"unresolved_image_url",
                "message":"Use an asset ID, an S3 locator, or an s3:// / HTTP object URL belonging to exactly one connected S3 source."}))).into_response());
        }
        id=remote_asset_id(&matches[0].0,&matches[0].1);
    }
    if let Some((source_id,key))=remote_asset_key(&id) {
        let response=load_s3_asset(State(state.clone()),Path(source_id),Json(S3AssetInput {key,width:None,height:None,bit_depth:None})).await;
        if !response.status().is_success() {return Err(response);}
    }
    match resolved_asset(state,&AssetId(id)).await {
        Ok(Some(asset))=>Ok(asset),Ok(None)=>Err(errors::reply(StatusCode::NOT_FOUND,"asset_not_found","The requested image is unavailable.",serde_json::json!({}))),
        Err(error)=>Err(errors::reply(StatusCode::UNPROCESSABLE_ENTITY,"image_header_failed",format!("Cannot read image header: {error:#}"),serde_json::json!({}))),
    }
}

fn source_key(image:&str,bucket:&str,region:&str,endpoint:Option<&str>)->Option<String> {
    let uri:axum::http::Uri=image.parse().ok()?;
    let scheme=uri.scheme_str()?;let authority=uri.authority()?.as_str();
    let key=if scheme=="s3" && authority==bucket {uri.path().trim_start_matches('/').to_owned()}
    else {
        if scheme!="https" && scheme!="http" {return None;}
        let endpoints=endpoint.map(|value|vec![value.to_owned()]).unwrap_or_else(||vec![
            format!("https://s3.{region}.amazonaws.com"),"https://s3.amazonaws.com".into()]);
        let mut found=None;
        for endpoint in endpoints {
            let base:axum::http::Uri=endpoint.parse().ok()?;
            if base.scheme_str()!=Some(scheme) {continue;}
            let host=base.authority()?.as_str();let prefix=base.path().trim_end_matches('/');
            let path_prefix=if authority.eq_ignore_ascii_case(host) {format!("{prefix}/{bucket}/")}
                else if authority.eq_ignore_ascii_case(&format!("{bucket}.{host}")) {format!("{prefix}/")}
                else {continue;};
            found=uri.path().strip_prefix(&path_prefix).map(str::to_owned);
            if found.is_some() {break;}
        }
        found?
    };
    decode_key(&key)
}

fn decode_key(key:&str)->Option<String> {
    let mut bytes=Vec::with_capacity(key.len());let mut input=key.as_bytes().iter().copied();
    while let Some(byte)=input.next() {
        if byte!=b'%' {bytes.push(byte);continue;}
        let high=(input.next()? as char).to_digit(16)?;let low=(input.next()? as char).to_digit(16)?;
        bytes.push((high*16+low) as u8);
    }
    String::from_utf8(bytes).ok().filter(|key|!key.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maps_connected_object_urls_without_arbitrary_url_fetches() {
        for url in ["s3://bucket/folder/a%20b+%D7%90.jp2","https://bucket.s3.eu-west-1.amazonaws.com/folder/a%20b+%D7%90.jp2?X-Amz-Signature=ignored",
            "https://s3.eu-west-1.amazonaws.com/bucket/folder/a%20b+%D7%90.jp2"] {
            assert_eq!(source_key(url,"bucket","eu-west-1",None).as_deref(),Some("folder/a b+א.jp2"));
        }
        assert_eq!(source_key("http://minio:9000/storage/bucket/image.jp2","bucket","us-east-1",Some("http://minio:9000/storage")).as_deref(),Some("image.jp2"));
        for url in ["https://unconnected.test/bucket/image.jp2","file:///etc/passwd","s3://other/image.jp2","s3://bucket/bad%ZZ.jp2"] {
            assert!(source_key(url,"bucket","eu-west-1",None).is_none());
        }
    }
}

#[derive(Clone,Copy,Debug)]
pub(super) struct Geometry { pub span:u32, pub bitmap:u32 }
impl Geometry {
    fn default_for(width:u32,height:u32,z:u8)->anyhow::Result<Self> {
        Self::parse(Some(256),Some(default_max_zoom(width,height)?),Some(0),Some(1.0),z)?
            .ok_or_else(||anyhow::anyhow!("Missing compatibility geometry"))
    }

    fn parse(size:Option<u32>,max:Option<u8>,min:Option<u8>,ratio:Option<f64>,z:u8)->anyhow::Result<Option<Self>> {
        if size.is_none() && max.is_none() && min.is_none() && ratio.is_none() {return Ok(None);}
        let (Some(size),Some(max))=(size,max) else {anyhow::bail!("Explicit geometry requires tileSize and maxZoom; minZoom defaults to 0 and tilePixelRatio to 1.");};
        let min=min.unwrap_or(0);let ratio=ratio.unwrap_or(1.0);
        anyhow::ensure!(size>0 && size<=4096,"tileSize must be between 1 and 4096 source pixels");
        anyhow::ensure!(min<=max && max<=30 && z>=min && z<=max,"zoom must be within minZoom..maxZoom, with 0 <= minZoom <= maxZoom <= 30");
        let bitmap=(f64::from(size)*ratio).round();
        anyhow::ensure!(ratio.is_finite() && ratio>0.0 && bitmap>=1.0 && bitmap<=4096.0,"tilePixelRatio must produce a bitmap edge between 1 and 4096 pixels");
        let span=size.checked_mul(1u32 << (max-z)).ok_or_else(||anyhow::anyhow!("source tile footprint exceeds the 32-bit coordinate limit"))?;
        Ok(Some(Self {span,bitmap:bitmap as u32}))
    }
}

#[cfg(test)]
mod geometry_tests {
    use super::*;
    #[test]
    fn independent_pyramid_and_density() {
        for (z,span) in [(7,256),(6,512),(5,1024),(0,32768)] {
            for (ratio,bitmap) in [(1.0,256),(2.0,512),(1.5,384),(0.5,128)] {
                let g=Geometry::parse(Some(256),Some(7),None,Some(ratio),z).unwrap().unwrap();
                assert_eq!((g.span,g.bitmap),(span,bitmap));
            }
        }
        assert!(Geometry::parse(None,None,None,None,0).unwrap().is_none());
        assert_eq!(Geometry::parse(Some(512),Some(20),Some(2),None,20).unwrap().unwrap().span,512);
    }
    #[test]
    fn rejects_incomplete_invalid_and_overflowing_geometry() {
        for (size,max,min,ratio,z) in [
            (Some(256),None,None,None,0),(None,Some(7),None,None,0),
            (None,None,Some(0),None,0),(None,None,None,Some(2.0),0),
            (Some(256),Some(7),None,None,8),(Some(256),Some(7),Some(3),None,2),
            (Some(256),Some(7),Some(8),None,7),(Some(0),Some(7),None,None,7),
            (Some(256),Some(7),None,Some(0.0),7),(Some(256),Some(7),None,Some(f64::NAN),7),
            (Some(512),Some(30),None,None,0),(Some(512),Some(7),None,Some(9.0),7),
        ] {assert!(Geometry::parse(size,max,min,ratio,z).is_err());}
    }
}

#[cfg(test)] mod default_pyramid_tests {
 use super::*;
 #[test] fn image_extent_defines_external_zoom_independently_of_codestream_levels() {
  for (extent,max) in [(1,0),(256,0),(257,1),(512,1),(513,2),(4096,4),(40980,8),(163840,10),(1u32<<31,23)] {
   assert_eq!(default_max_zoom(extent,1).unwrap(),max);
   for z in 0..=max {let g=Geometry::default_for(extent,1,z).unwrap();assert_eq!((g.span,g.bitmap),(256u32<<(max-z),256));}
   assert!(Geometry::default_for(extent,1,max+1).is_err());
  }
 }
 #[test] fn invalid_extents_fail_and_explicit_geometry_remains_independent() {
  assert!(default_max_zoom(0,1).is_err());assert!(default_max_zoom(1,0).is_err());assert!(default_max_zoom(u32::MAX,1).is_err());
  let g=Geometry::parse(Some(512),Some(6),Some(0),Some(2.0),6).unwrap().unwrap();assert_eq!((g.span,g.bitmap),(512,1024));
 }
}
