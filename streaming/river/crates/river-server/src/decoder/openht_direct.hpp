// Bounded direct classicPart1 decoding for canonical zeroDWT packed views.
// Calls the pinned OpenHTJ2K BSD-3-Clause classic Part1 block decoder.
#include "block_decoding.hpp"
#include <algorithm>
#include <atomic>
#include <thread>
#include <mutex>
#include <exception>
#include <chrono>
#include <cmath>
#include <cstring>
#include <fstream>
#include <iostream>
#include <stdexcept>
#include <vector>

namespace river_direct {
using B=std::vector<uint8_t>;
void check(bool ok,const char*m){if(!ok)throw std::runtime_error(m);}
struct Unsupported:std::runtime_error {using std::runtime_error::runtime_error;};
void supported(bool ok,const char*m){if(!ok)throw Unsupported(m);}
uint16_t u16(const B&b,size_t p){check(p+2<=b.size(),"short u16");return (uint16_t(b[p])<<8)|b[p+1];}
uint32_t u32(const B&b,size_t p){check(p+4<=b.size(),"short u32");return uint32_t(u16(b,p))<<16|u16(b,p+2);}
struct Bits {
 const B&b;size_t at,end;uint8_t byte=0,previous=0,left=0;
 uint32_t bit(){if(!left){check(at<end,"short packet header");byte=b[at++];left=previous==255?7:8;check(previous!=255 || !(byte&128),"invalid stuffed packet bit");previous=byte;}return (byte>>--left)&1;}
 uint32_t bits(unsigned n){check(n<=32,"oversized packet field");uint32_t v=0;while(n--)v=(v<<1)|bit();return v;}
 size_t finish(){if(previous==255){check(at<end && !(b[at]&128),"short final stuffed byte");++at;}return at;}
};
inline void decode(B&b,unsigned threads,uint32_t expected_w,uint32_t expected_h,B&rgba,bool force=false){
 auto began=std::chrono::steady_clock::now();check(u16(b,0)==0xff4f,"raw codestream required");
 uint32_t w=0,h=0,pw=0,ph=0,cw=0,ch=0;uint16_t nc=0;uint8_t depth=0,mb=0;bool signedness=false,siz=false,cod=false,qcd=false;size_t p=2;
 while(u16(b,p)!=0xff90){auto marker=u16(b,p),len=u16(b,p+2);check(len>=2 && p+2+len<=b.size(),"invalid header marker");
  if(marker==0xff51){check(!siz && len>=41,"invalid SIZ");siz=true;w=u32(b,p+6);h=u32(b,p+10);nc=u16(b,p+38);supported(nc>=1 && nc<=4 && len==38+3*nc,"unsupported components");check(w && h && uint64_t(w)*h<=4*1024*1024,"pixel limit");supported(!u32(b,p+14)&&!u32(b,p+18)&&!u32(b,p+30)&&!u32(b,p+34)&&u32(b,p+22)==w&&u32(b,p+26)==h,"unsupported image or tile origin");
   depth=(b[p+40]&127)+1;signedness=(b[p+40]&128)!=0;supported(depth<=16,"precision limit");for(unsigned c=0;c<nc;++c)supported(b[p+40+c*3]==b[p+40]&&b[p+41+c*3]==1&&b[p+42+c*3]==1,"nonuniform components");
  }else if(marker==0xff52){check(!cod && len==13,"invalid COD");cod=true;auto c=p+4;check(b[c]==1&&b[c+1]==2&&u16(b,c+2)==1&&!b[c+4]&&!b[c+5]&&!b[c+8]&&b[c+9]==1,"unsupported coding mode");supported(b[c+6]<=4&&b[c+7]<=4,"codeblock limit");cw=1u<<(b[c+6]+2);ch=1u<<(b[c+7]+2);pw=1u<<(b[c+10]&15);ph=1u<<(b[c+10]>>4);supported(pw<=cw&&ph<=ch,"multiple codeblocks per precinct unsupported");
  }else if(marker==0xff5c){check(!qcd&&len==4&&!(b[p+4]&31),"unsupported QCD");qcd=true;auto exponent=b[p+5]>>3,guard=b[p+4]>>5;check(exponent+guard>=1&&exponent+guard<=31,"invalid magnitude precision");mb=exponent+guard-1;
  }else check(marker==0xff64,"unsupported main header");p+=len+2;
 }
 check(siz&&cod&&qcd,"required header missing");check(w==expected_w&&h==expected_h,"worker raster dimension mismatch");check(u16(b,p+2)==10&&!u16(b,p+4)&&!b[p+10]&&b[p+11]==1&&u16(b,p+12)==0xff93,"unsupported tile part");auto part=u32(b,p+6);check(part>=14&&p+part+2==b.size()&&u16(b,p+part)==0xffd9,"invalid tile extent");size_t end=p+part;p+=14;
 const uint64_t packet_count=uint64_t((w+pw-1)/pw)*((h+ph-1)/ph)*nc;
 check(packet_count<=524288,"packet limit");
 struct Desc{size_t at;uint32_t x,y;uint16_t length;uint8_t c,w,h,zbp,passes;};std::vector<Desc> descriptors;descriptors.reserve(size_t((w+pw-1)/pw)*((h+ph-1)/ph)*nc);size_t packets=0,blocks=0;
 for(uint32_t y=0;y<h;y+=ph)for(uint32_t x=0;x<w;x+=pw)for(uint16_t c=0;c<nc;++c){
  ++packets;Bits bits{b,p,end};if(!bits.bit()){p=bits.finish();descriptors.push_back(Desc{p,x,y,0,uint8_t(c),uint8_t(std::min(pw,w-x)),uint8_t(std::min(ph,h-y)),0,0});continue;}if(!bits.bit()){p=bits.finish();descriptors.push_back(Desc{p,x,y,0,uint8_t(c),uint8_t(std::min(pw,w-x)),uint8_t(std::min(ph,h-y)),0,0});continue;}unsigned zbp=0;while(!bits.bit()){check(++zbp<32,"invalid zero bitplanes");}
  unsigned passes;if(!bits.bit())passes=1;else if(!bits.bit())passes=2;else {unsigned v=bits.bits(2);if(v<3)passes=3+v;else {v=bits.bits(5);passes=v<31?6+v:37+bits.bits(7);}}
  check(zbp<mb&&passes<=3*(unsigned(mb)-zbp)-2,"invalid coding pass count");unsigned lb=3;while(bits.bit())check(++lb<=31,"invalid length bits");unsigned log=0;for(unsigned v=passes;v>1;v>>=1)++log;auto length=bits.bits(lb+log);p=bits.finish();check(p+length<=end,"invalid block body");supported(length>0&&length<=65535,"unsupported direct block body length");
  descriptors.push_back(Desc{p,x,y,uint16_t(length),uint8_t(c),uint8_t(std::min(pw,w-x)),uint8_t(std::min(ph,h-y)),uint8_t(zbp),uint8_t(passes)});
  p+=length;++blocks;
 }
 check(p==end,"packet bodies do not cover tile");auto parsed=std::chrono::steady_clock::now();
 const int64_t maximum=(int64_t(1)<<depth)-1,shift=int64_t(1)<<(depth-1);
 const uint8_t neutral=uint8_t((shift*255+maximum/2)/maximum);
 rgba.assign(size_t(w)*h*4,neutral);
 if(nc==1 || nc==3)for(size_t i=3;i<rgba.size();i+=4)rgba[i]=255;
 // Every group owns all components of complete precincts, including empty
 // packets. Workers therefore never write different channels of one pixel.
 const size_t grain=32*nc;
 std::atomic<size_t> next{0};std::atomic<bool> stopped{false};std::exception_ptr error;std::mutex error_mutex;
 auto worker=[&]{try{
 std::vector<int32_t> samples(64*64);std::vector<uint8_t> states(66*66);std::vector<uint32_t> contexts((64/4+2)*66);std::vector<sprec_t> band(64*64);
 while(!stopped.load(std::memory_order_relaxed)){
  auto begin=next.fetch_add(grain,std::memory_order_relaxed);if(begin>=descriptors.size())break;
  for(auto k=begin;k<std::min(begin+grain,descriptors.size());++k){
   const auto&d=descriptors[k];if(d.passes==0)continue;auto x=d.x,y=d.y;uint16_t c=d.c;unsigned zbp=d.zbp,passes=d.passes;uint32_t length=d.length;size_t p=d.at;
  auto bw=std::min(pw,w-x),bh=std::min(ph,h-y),sx=(bw+7)&~7u,sy=(bh+7)&~7u;
  std::fill(samples.begin(),samples.begin()+sx*sy,0);std::fill(states.begin(),states.begin()+(sx+2)*(sy+2),0);std::fill(contexts.begin(),contexts.begin()+(sy/4+2)*(sx+2),0);
  uint8_t layer_start=0,layer_passes=uint8_t(passes);j2k_codeblock block(0,0,mb,0,1,1.0f,bw,band.data(),0,1,0,element_siz(0,0),element_siz(bw,bh),element_siz(bw,bh));
  block.sample_buf=samples.data();block.blksampl_stride=sx;block.block_states=states.data();block.blkstate_stride=sx+2;block.block_contexts=contexts.data();block.block_contexts_stride=sx+2;block.layer_start=&layer_start;block.layer_passes=&layer_passes;block.num_passes=uint8_t(passes);block.num_ZBP=uint8_t(zbp);block.length=length;block.dequant_i32=true;block.ensure_pass_capacity(uint8_t(passes));std::fill(block.pass_length,block.pass_length+passes,0);block.pass_length[0]=length;block.pass_length_count=uint8_t(passes);block.set_compressed_data(b.data()+p,uint16_t(length));j2k_decode(&block,0);
  for(uint32_t j=0;j<bh;++j)for(uint32_t i=0;i<bw;++i){
   int32_t value;static_assert(sizeof(sprec_t)==4);std::memcpy(&value,&band[j*bw+i],4);
   auto v=std::clamp<int64_t>(int64_t(value)+shift,0,maximum);
   auto normalized=depth==8?uint8_t(v):uint8_t((v*255+maximum/2)/maximum);
   auto out=&rgba[((size_t(y)+j)*w+x+i)*4];
   if(nc<3 && c==0){out[0]=out[1]=out[2]=normalized;}
   else out[nc==2?3:c]=normalized;
  }
  }
 }
 }catch(...){stopped.store(true,std::memory_order_relaxed);std::lock_guard<std::mutex> lock(error_mutex);if(!error)error=std::current_exception();}};
 std::vector<std::thread> jobs;if(descriptors.size()<128)threads=1;jobs.reserve(threads-1);try{for(unsigned i=1;i<threads;++i)jobs.emplace_back(worker);worker();}catch(...){stopped.store(true,std::memory_order_relaxed);for(auto&t:jobs)if(t.joinable())t.join();throw;}for(auto&t:jobs)t.join();if(error)std::rethrow_exception(error);auto decoded=std::chrono::steady_clock::now();


}
}
