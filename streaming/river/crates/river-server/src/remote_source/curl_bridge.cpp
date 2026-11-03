// Exclusive easy/multi pair. The Rust owner bounds the number of leased pairs.
#include <curl/curl.h>
#include <algorithm>
#include <charconv>
#include <chrono>
#include <cstdint>
#include <cstring>
#include <mutex>
#include <new>
#include <string>
#include <string_view>
extern "C" {
struct RiverCurlRequest {
 const char *url,*signing,*userpwd,*token,*etag,*version,*ca_info;
 uint64_t start,end,total,timeout_ms;
};
struct RiverCurlResponse {uint64_t written;int64_t status;int32_t curl_code,violation;uint64_t requests,retry_after_ms;};
using Cancel=int(*)(void*);
uint32_t river_curl_abi(){return 3;}
uint32_t river_curl_request_size(){return sizeof(RiverCurlRequest);}
uint32_t river_curl_response_size(){return sizeof(RiverCurlResponse);}
const char* river_curl_error_text(int code){return curl_easy_strerror(static_cast<CURLcode>(code));}
}
namespace {
struct Handle {CURL* easy=nullptr;CURLM* multi=nullptr;~Handle(){if(easy)curl_easy_cleanup(easy);if(multi)curl_multi_cleanup(multi);}};
struct Cleanup {Handle& h;curl_slist* headers=nullptr;bool added=false;~Cleanup(){if(added)curl_multi_remove_handle(h.multi,h.easy);curl_easy_reset(h.easy);curl_slist_free_all(headers);}};
struct Transfer {const RiverCurlRequest* request;unsigned char* out;uint64_t written=0,requests=0;long status=0;bool range=false,etag=false,version=false,invalid=false,headers=false;int violation=0;};
int count_request(CURL* easy,curl_infotype type,char*,size_t,void* opaque){
 // Deliberately do not read, retain or print debug bytes: they contain auth.
 char* owner=nullptr;curl_easy_getinfo(easy,CURLINFO_PRIVATE,&owner);
 if(owner==opaque&&type==CURLINFO_HEADER_OUT)++static_cast<Transfer*>(opaque)->requests;return 0;
}
bool nonempty(const char* p){return p && *p;}
std::string_view trim(std::string_view v){while(!v.empty() && (v.front()==' ' || v.front()=='\t'))v.remove_prefix(1);while(!v.empty() && (v.back()=='\r'||v.back()=='\n'||v.back()==' '||v.back()=='\t'))v.remove_suffix(1);return v;}
bool eq(std::string_view a,std::string_view b){if(a.size()!=b.size())return false;for(size_t i=0;i<a.size();++i){char c=a[i];if(c>='A'&&c<='Z')c+=32;if(c!=b[i])return false;}return true;}
bool range_matches(std::string_view v,const RiverCurlRequest& r){
 if(v.substr(0,6)!="bytes ")return false;v.remove_prefix(6);uint64_t a=0,b=0,n=0;
 auto parse=[&](uint64_t& out){auto x=std::from_chars(v.data(),v.data()+v.size(),out);if(x.ec!=std::errc{})return false;v.remove_prefix(x.ptr-v.data());return true;};
 if(!parse(a)||v.empty()||v.front()!='-')return false;v.remove_prefix(1);
 if(!parse(b)||v.empty()||v.front()!='/')return false;v.remove_prefix(1);
 return parse(n)&&v.empty()&&a==r.start&&b==r.end&&n==r.total;
}
size_t receive_header(char* p,size_t size,size_t count,void* opaque){
 if(size && count>65536/size)return 0;size_t n=size*count;auto& t=*static_cast<Transfer*>(opaque);std::string_view line(p,n);const auto& r=*t.request;
 if(line.substr(0,5)=="HTTP/"){
  auto at=line.find(' ');if(at==std::string_view::npos){t.invalid=true;return 0;}
  long status=0;auto result=std::from_chars(line.data()+at+1,line.data()+line.size(),status);if(result.ec!=std::errc{}){t.invalid=true;return 0;}
  t.status=status;t.range=false;t.etag=!nonempty(r.etag);t.version=!nonempty(r.version);t.headers=false;t.violation=0;return n;
 }
 if(trim(line).empty()){
  if(t.status>=100&&t.status<200)return n;
  t.headers=t.status==206&&t.range&&t.etag&&t.version&&!t.invalid;
  if(!t.headers){if(t.status==206){t.invalid=true;if(!t.violation)t.violation=!t.range?1:!t.etag?2:!t.version?3:4;}return 0;}return n;
 }
 auto at=line.find(':');if(at==std::string_view::npos)return n;auto key=line.substr(0,at);auto value=trim(line.substr(at+1));
 if(eq(key,"content-range")){t.range=range_matches(value,r);if(!t.range){t.invalid=true;t.violation=1;}}
 if(eq(key,"etag")&&nonempty(r.etag)){t.etag=value==r.etag;if(!t.etag){t.invalid=true;t.violation=2;}}
 if(eq(key,"x-amz-version-id")&&nonempty(r.version)){t.version=value==r.version;if(!t.version){t.invalid=true;t.violation=3;}}
 if(eq(key,"content-length")){uint64_t length=0;auto x=std::from_chars(value.data(),value.data()+value.size(),length);if(x.ec!=std::errc{}||x.ptr!=value.data()+value.size()||length!=r.end-r.start+1){t.invalid=true;t.violation=4;}}
 return n;
}
size_t receive_body(char* p,size_t size,size_t count,void* opaque){
 auto& t=*static_cast<Transfer*>(opaque);uint64_t remaining=t.request->end-t.request->start+1-t.written;
 if(!t.headers || (size && count>remaining/size)){t.invalid=true;t.violation=5;return 0;}
 size_t n=size*count;std::memcpy(t.out+t.written,p,n);t.written+=n;return n;
}
}
extern "C" void* river_curl_create(){
 try {
 static std::once_flag once;static CURLcode initialized=CURLE_FAILED_INIT;
 std::call_once(once,[]{initialized=curl_global_init(CURL_GLOBAL_DEFAULT);});if(initialized!=CURLE_OK)return nullptr;
 auto* version=curl_version_info(CURLVERSION_NOW);if(!version||version->version_num<0x075500||!(version->features&CURL_VERSION_ASYNCHDNS))return nullptr;
 auto* h=new(std::nothrow) Handle;if(!h)return nullptr;h->easy=curl_easy_init();h->multi=curl_multi_init();
 if(!h->easy||!h->multi){delete h;return nullptr;}
 curl_multi_setopt(h->multi,CURLMOPT_MAXCONNECTS,4L);curl_multi_setopt(h->multi,CURLMOPT_MAX_TOTAL_CONNECTIONS,4L);return h;
 }catch(...){return nullptr;}
}
extern "C" void river_curl_destroy(void* p){delete static_cast<Handle*>(p);}
// 0 success,1 cancelled,2 timeout,3 HTTP status,4 invalid range/body,5 curl,6 input,7 setup.
static int fetch_impl(void* opaque,const RiverCurlRequest* req,unsigned char* out,size_t capacity,Cancel cancel,void* cancellation,RiverCurlResponse* response){
 if(!response)return 6;*response={};
 if(!opaque||!req||!out||!nonempty(req->url)||!nonempty(req->signing)||!nonempty(req->userpwd)||req->start>req->end||req->end>=req->total||req->end-req->start>=64*1024*1024||capacity<req->end-req->start+1||req->timeout_ms==0||req->timeout_ms>10000||(!nonempty(req->etag)&&!nonempty(req->version)))return 6;
 if(cancel&&cancel(cancellation))return 1;
 auto& h=*static_cast<Handle*>(opaque);curl_easy_reset(h.easy);Transfer transfer{req,out};Cleanup cleanup{h};int result=0;CURLcode code=CURLE_OK;
 auto finish=[&](int value){response->written=transfer.written;long status=0;curl_off_t retry=0;curl_easy_getinfo(h.easy,CURLINFO_RESPONSE_CODE,&status);curl_easy_getinfo(h.easy,CURLINFO_RETRY_AFTER,&retry);response->status=status?status:transfer.status;response->curl_code=code;response->violation=transfer.violation;response->requests=transfer.requests;response->retry_after_ms=retry>0?static_cast<uint64_t>(std::min<curl_off_t>(retry,10))*1000:0;return value;};
 auto append=[&](const std::string& value){auto* next=curl_slist_append(cleanup.headers,value.c_str());if(!next)return false;cleanup.headers=next;return true;};
 if(nonempty(req->etag)&&!append(std::string("If-Match: ")+req->etag))return finish(7);
 if(nonempty(req->token)&&!append(std::string("x-amz-security-token: ")+req->token))return finish(7);
 std::string range=std::to_string(req->start)+"-"+std::to_string(req->end);
#define SET(option,value) do{code=curl_easy_setopt(h.easy,option,value);if(code!=CURLE_OK)return finish(7);}while(0)
 SET(CURLOPT_URL,req->url);SET(CURLOPT_RANGE,range.c_str());SET(CURLOPT_HTTPGET,1L);
 SET(CURLOPT_USERAGENT,"River/native-curl");
 SET(CURLOPT_AWS_SIGV4,req->signing);SET(CURLOPT_USERPWD,req->userpwd);SET(CURLOPT_HTTPHEADER,cleanup.headers);
 SET(CURLOPT_PROTOCOLS_STR,"http,https");SET(CURLOPT_REDIR_PROTOCOLS_STR,"http,https");SET(CURLOPT_FOLLOWLOCATION,0L);SET(CURLOPT_PATH_AS_IS,1L);
 SET(CURLOPT_SSL_VERIFYPEER,1L);SET(CURLOPT_SSL_VERIFYHOST,2L);if(nonempty(req->ca_info))SET(CURLOPT_CAINFO,req->ca_info);
 SET(CURLOPT_PROXY,"");SET(CURLOPT_NOSIGNAL,1L);SET(CURLOPT_TCP_NODELAY,1L);SET(CURLOPT_TIMEOUT_MS,static_cast<long>(req->timeout_ms));SET(CURLOPT_CONNECTTIMEOUT_MS,static_cast<long>(req->timeout_ms));
 SET(CURLOPT_WRITEFUNCTION,receive_body);SET(CURLOPT_WRITEDATA,&transfer);SET(CURLOPT_HEADERFUNCTION,receive_header);SET(CURLOPT_HEADERDATA,&transfer);
 SET(CURLOPT_PRIVATE,&transfer);SET(CURLOPT_DEBUGFUNCTION,count_request);SET(CURLOPT_DEBUGDATA,&transfer);SET(CURLOPT_VERBOSE,1L);
#undef SET
 auto deadline=std::chrono::steady_clock::now()+std::chrono::milliseconds(req->timeout_ms);
 if(curl_multi_add_handle(h.multi,h.easy)!=CURLM_OK)return finish(7);cleanup.added=true;int running=0;
 if(curl_multi_perform(h.multi,&running)!=CURLM_OK)return finish(5);
 while(running){
  if(cancel&&cancel(cancellation)){result=1;break;}
  if(std::chrono::steady_clock::now()>=deadline){result=2;break;}
  if(curl_multi_poll(h.multi,nullptr,0,10,nullptr)!=CURLM_OK||curl_multi_perform(h.multi,&running)!=CURLM_OK){result=5;break;}
 }
 if(!result){int queued=0;bool done=false;while(auto* msg=curl_multi_info_read(h.multi,&queued)){if(msg->msg==CURLMSG_DONE&&msg->easy_handle==h.easy){code=msg->data.result;done=true;}}
  if(!done)result=5;
  else if(code==CURLE_OPERATION_TIMEDOUT)result=2;
  else if(transfer.status&&transfer.status!=206)result=3;
  else if(code!=CURLE_OK&&transfer.status==0)result=5;
  else if(transfer.invalid||!transfer.headers||transfer.written!=req->end-req->start+1){result=4;if(!transfer.violation)transfer.violation=6;}
  else if(code!=CURLE_OK)result=5;
 }
 return finish(result);
}
extern "C" int river_curl_fetch(void* opaque,const RiverCurlRequest* req,unsigned char* out,size_t capacity,Cancel cancel,void* cancellation,RiverCurlResponse* response){
 try{return fetch_impl(opaque,req,out,capacity,cancel,cancellation,response);}
 catch(...){if(response){*response={};response->curl_code=CURLE_OUT_OF_MEMORY;}return 7;}
}
