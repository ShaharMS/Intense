// Research worker: one persistent decoder owns its thread pool in this process.
// IPC is private, bounded, versioned and independent of decoder stdout.
#include "decoder.hpp"
#include "openht_direct.hpp"
#include <algorithm>
#include <array>
#include <csignal>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <iostream>
#include <stdexcept>
#include <thread>
#include <vector>
#include <sys/resource.h>
#include <unistd.h>
#ifdef __linux__
#include <sys/prctl.h>
#endif
constexpr uint32_t REQUEST=0x31544852, RESPONSE=0x314f4852;
constexpr size_t MAX_INPUT=66*1024*1024, MAX_PIXELS=4*1024*1024;
uint32_t le32(const uint8_t*p){return uint32_t(p[0])|uint32_t(p[1])<<8|uint32_t(p[2])<<16|uint32_t(p[3])<<24;}
uint16_t be16(const uint8_t*p){return uint16_t(uint16_t(p[0])<<8|p[1]);}
void put32(uint8_t*p,uint32_t x){for(int i=0;i<4;++i)p[i]=uint8_t(x>>(8*i));}
bool read_exact(int fd,void*out,size_t n,bool clean_eof=false){
 auto p=static_cast<uint8_t*>(out);size_t done=0;
 while(done<n){auto count=read(fd,p+done,n-done);if(count<0&&errno==EINTR)continue;
  if(count==0&&done==0&&clean_eof)return false;if(count<=0)throw std::runtime_error("truncated worker request");done+=size_t(count);}
 return true;
}
void write_exact(int fd,const void*data,size_t n){
 auto p=static_cast<const uint8_t*>(data);while(n){auto count=write(fd,p,n);if(count<0&&errno==EINTR)continue;
  if(count<=0)throw std::runtime_error("worker response connection closed");p+=count;n-=size_t(count);}
}
size_t canonical_prefix(const std::vector<uint8_t>&b,size_t size,uint32_t components,uint32_t width,uint32_t height){
 if(size<48||b[0]!=255||b[1]!=79)throw std::runtime_error("worker requires a raw packed codestream");
 size_t p=2;bool siz=false,cod=false,qcd=false;
 while(p+4<=size && be16(&b[p])!=0xff90){
  auto marker=be16(&b[p]);auto length=be16(&b[p+2]);if(length<2||p+2+length>size||p>1024*1024)throw std::runtime_error("invalid packed header length");
  if(marker==0xff51){if(siz)throw std::runtime_error("duplicate packed SIZ");siz=true;}
  else if(marker==0xff52){
   if(cod||length!=13)throw std::runtime_error("unsupported packed COD");cod=true;const auto c=&b[p+4];
   if(c[0]!=1||c[1]!=2||be16(c+2)!=1||c[4]>1||c[5]!=0||c[8]!=0||c[9]!=1)throw std::runtime_error("unsupported packed coding mode");
   auto pw=uint64_t(1)<<(c[10]&15),ph=uint64_t(1)<<(c[10]>>4);
   auto packets=((width+pw-1)/pw)*((height+ph-1)/ph)*components;
   if(packets>524288)throw std::runtime_error("packed packet count exceeds worker limit");
  }else if(marker==0xff5c){if(qcd||length!=4||(b[p+4]&31)!=0)throw std::runtime_error("unsupported packed quantization");qcd=true;}
  else if(marker==0xff5d){if(length!=5||(b[p+5]&31)!=0)throw std::runtime_error("unsupported coarse component quantization");}
  else if(marker!=0xff64)throw std::runtime_error("unsupported packed header marker");
  p+=2+length;
 }
 if(!siz||!cod||!qcd||p+14>size||be16(&b[p])!=0xff90||be16(&b[p+2])!=10||be16(&b[p+4])!=0||b[p+10]!=0||b[p+11]!=1||be16(&b[p+12])!=0xff93)throw std::runtime_error("invalid packed tile header");
 const auto length=(uint32_t(b[p+6])<<24)|(uint32_t(b[p+7])<<16)|(uint32_t(b[p+8])<<8)|b[p+9];
 if(length<14||p+length+2!=size||be16(&b[size-2])!=0xffd9)throw std::runtime_error("invalid packed tile extent");
 return p;
}
bool direct_eligible(const std::vector<uint8_t>&b,size_t prefix){
 for(size_t at=2;at<prefix;at+=size_t(be16(&b[at+2]))+2){
  const auto marker=be16(&b[at]);
  if(marker==0xff5d)return false;
  if(marker==0xff52){const auto c=&b[at+4];
   if(c[4]!=0||c[6]>4||c[7]>4||(c[10]&15)>c[6]+2||(c[10]>>4)>c[7]+2)return false;
  }
 }
 return true;
}
void response(int fd,uint32_t status,uint32_t w,uint32_t h,uint64_t sequence,bool reused,const uint8_t*data,size_t n){
 std::array<uint8_t,32> header{};const uint32_t fields[]={RESPONSE,status,w,h,uint32_t(n),uint32_t(sequence),uint32_t(sequence>>32),uint32_t(reused)};
 for(size_t i=0;i<8;++i)put32(header.data()+4*i,fields[i]);write_exact(fd,header.data(),header.size());write_exact(fd,data,n);
}
[[noreturn]] void failure(int fd,uint64_t sequence,const char*message){
 try{response(fd,1,0,0,sequence,false,reinterpret_cast<const uint8_t*>(message),std::min<size_t>(std::strlen(message),4096));}catch(...){}
 std::_Exit(1);
}
int main(int argc,char**argv){
 if(argc==2 && std::strcmp(argv[1],"--self-test")==0) {std::puts("river-openht-worker protocol1");return 0;}
 if(argc==2 && std::strcmp(argv[1],"--frame-threads")==0)return 0;
 if(argc!=4)return 2;auto threads=std::strtoul(argv[1],nullptr,10),memory=std::strtoul(argv[2],nullptr,10),parent=std::strtoul(argv[3],nullptr,10);
 if(threads<1||threads>16||memory<256||memory>4096||parent<1)return 3;
#ifdef __linux__
 if(prctl(PR_SET_PDEATHSIG,SIGKILL)||getppid()!=pid_t(parent))return 4;
#endif
 rlimit limit{rlim_t(memory)*1024*1024,rlim_t(memory)*1024*1024};if(setrlimit(RLIMIT_AS,&limit))return 5;
 std::signal(SIGPIPE,SIG_IGN);int protocol=dup(STDOUT_FILENO);if(protocol<0||dup2(STDERR_FILENO,STDOUT_FILENO)<0)return 6;
 setvbuf(stdout,nullptr,_IONBF,0);
 uint64_t sequence=0,last_sequence=0;
 try{
  open_htj2k::openhtj2k_decoder decoder;decoder.enable_single_tile_reuse(true);
  std::vector<uint8_t> active,incoming,rgba,last_prefix;
  while(true){
   try{
   std::array<uint8_t,32> header{};if(!read_exact(STDIN_FILENO,header.data(),header.size(),true))return 0;
   auto size=le32(header.data()+4),width=le32(header.data()+8),height=le32(header.data()+12);sequence=uint64_t(le32(header.data()+16))|uint64_t(le32(header.data()+20))<<32;
   if(le32(header.data())!=REQUEST)throw std::runtime_error("unsupported packed worker protocol version");
   auto frame_threads=le32(header.data()+24),flags=le32(header.data()+28);
   if(flags==0 && frame_threads==0)frame_threads=uint32_t(threads);
   else if(flags!=0x54485231 || frame_threads<1 || frame_threads>threads)throw std::runtime_error("unsupported packed worker request flags or thread budget");
   if(sequence!=last_sequence+1)throw std::runtime_error("packed worker request sequence mismatch");
   if(size>MAX_INPUT||size<48)throw std::runtime_error("packed worker input length is outside48bytes..66MiB");
   if(!width||!height||uint64_t(width)*height>MAX_PIXELS)throw std::runtime_error("packed worker raster exceeds4millionpixels or has an empty dimension");
   incoming.resize(size_t(size)+16);read_exact(STDIN_FILENO,incoming.data(),size);std::fill(incoming.begin()+size,incoming.end(),0);
   // Keep the previous input alive until its tile resources have been reset.
   const size_t prefix=canonical_prefix(incoming,size,1,width,height);
   const char*direct_mode=std::getenv("RIVER_OPENHT_DIRECT_BLOCKS");
   if(direct_eligible(incoming,prefix) && (!direct_mode || (std::strcmp(direct_mode,"false")!=0 && std::strcmp(direct_mode,"0")!=0))){
    try{
     std::vector<uint8_t> view(incoming.begin(),incoming.begin()+size);
     river_direct::decode(view,frame_threads,width,height,rgba,direct_mode && (std::strcmp(direct_mode,"true")==0 || std::strcmp(direct_mode,"1")==0));
     last_sequence=sequence;response(protocol,0,width,height,sequence,false,rgba.data(),rgba.size());
     // The legacy decoder may still borrow active from an earlier fallback.
     // Direct input is not retained and must not replace that live buffer.
     if(incoming.capacity()>8*1024*1024 && size<8*1024*1024)std::vector<uint8_t>().swap(incoming);
     continue;
    }catch(const river_direct::Unsupported&){
     // Valid packed modes outside the direct decoder keep the prior path.
    }
   }

   const bool reused=last_prefix.size()==prefix && std::equal(last_prefix.begin(),last_prefix.end(),incoming.begin());
   if(!reused){decoder.enable_single_tile_reuse(false);decoder.enable_single_tile_reuse(true);}
   decoder.init_borrow(incoming.data(),size,0,uint32_t(threads));decoder.parse();
   auto nc=decoder.get_num_component();if(nc<1||nc>4||decoder.get_mct()>1)throw std::runtime_error("unsupported packed worker components");
   canonical_prefix(incoming,size,nc,width,height);
   std::array<uint8_t,4> depth{};std::array<bool,4> signedness{};
   for(uint16_t c=0;c<nc;++c){depth[c]=decoder.get_component_depth(c);signedness[c]=decoder.get_component_signedness(c);
    if(depth[c]<1||depth[c]>16||decoder.get_component_width(c)!=width||decoder.get_component_height(c)!=height)throw std::runtime_error("packed worker component geometry mismatch");}
   rgba.resize(size_t(width)*height*4);std::vector<uint8_t> seen(height,0);uint32_t rows=0;const auto owner=std::this_thread::get_id();
   std::vector<uint32_t>w,h;std::vector<uint8_t>d;std::vector<bool>s;
   decoder.invoke_line_based_stream_reuse([&](uint32_t y,int32_t*const*planes,uint16_t count){
    if(std::this_thread::get_id()!=owner||y>=height||count!=nc||seen[y])throw std::runtime_error("unexpected packed worker output row");
    seen[y]=1;++rows;
    for(uint32_t x=0;x<width;++x){std::array<uint8_t,4> values{};
     for(uint16_t c=0;c<nc;++c){int64_t maximum=(int64_t(1)<<depth[c])-1;int64_t value=planes[c][x]+(signedness[c]?(int64_t(1)<<(depth[c]-1)):0);
      value=std::clamp<int64_t>(value,0,maximum);values[c]=uint8_t((value*255+maximum/2)/maximum);}
     auto out=&rgba[(size_t(y)*width+x)*4];out[0]=values[0];out[1]=nc<3?values[0]:values[1];out[2]=nc<3?values[0]:values[2];out[3]=nc==2?values[1]:nc==4?values[3]:255;
    }
   },w,h,d,s);
   if(rows!=height)throw std::runtime_error("incomplete packed worker raster");
   last_prefix.assign(incoming.begin(),incoming.begin()+prefix);last_sequence=sequence;response(protocol,0,width,height,sequence,reused,rgba.data(),rgba.size());active.swap(incoming);
   if(incoming.capacity()>8*1024*1024 && size<8*1024*1024)std::vector<uint8_t>().swap(incoming);
   }catch(const std::exception&e){failure(protocol,sequence,e.what());}
   catch(...){failure(protocol,sequence,"native packed decoder failed");}
  }
 }catch(const std::exception&e){
  failure(protocol,sequence,e.what());
 }catch(...){failure(protocol,sequence,"native packed decoder failed");}
 // A decoder failure can leave native tasks active. Exit the owned process
 // rather than recycling a partially failed object or unwinding its buffers.
 std::_Exit(1);
}
