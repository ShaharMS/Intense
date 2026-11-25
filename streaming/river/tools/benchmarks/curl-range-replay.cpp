// Diagnostic only: replay exact fixture ranges, without JP2 planning/decoding.
// This does not implement River's cancellation or production S3 error handling.
#include <curl/curl.h>
#include <openssl/evp.h>
#include <chrono>
#include <cctype>
#include <cstdint>
#include <ctime>
#include <iomanip>
#include <iostream>
#include <sstream>
#include <string>
#include <vector>
struct State {EVP_MD_CTX* digest;uint64_t bytes=0,expected=0,total=0,start=0,end=0;bool range_ok=false,etag_ok=false;std::string etag;std::vector<unsigned char> buffer;};
static size_t body(char* data,size_t size,size_t n,void* opaque) {
 auto& s=*static_cast<State*>(opaque);auto count=size*n;
 if(count>s.expected-s.bytes)return 0;
 s.buffer.insert(s.buffer.end(),data,data+count);
 s.bytes+=count;return count;
}
static size_t header(char* data,size_t size,size_t n,void* opaque) {
 auto& s=*static_cast<State*>(opaque);auto count=size*n;std::string line(data,count);
 auto at=line.find(':');if(at==std::string::npos)return count;
 auto key=line.substr(0,at);for(auto& c:key)c=static_cast<char>(std::tolower(static_cast<unsigned char>(c)));
 auto value=line.substr(at+1);auto first=value.find_first_not_of(" \t");if(first!=std::string::npos)value.erase(0,first);
 auto last=value.find_last_not_of("\r\n \t");if(last!=std::string::npos)value.erase(last+1);
 if(key=="etag")s.etag_ok=value==s.etag;
 if(key=="content-range") {std::string unit;uint64_t a=0,b=0,t=0;char dash=0,slash=0;std::istringstream in(value);in>>unit>>a>>dash>>b>>slash>>t;s.range_ok=in && unit=="bytes" && dash=='-' && slash=='/' && a==s.start && b==s.end && t==s.total;}
 return count;
}
static double cpu_ms(){timespec ts{};clock_gettime(CLOCK_PROCESS_CPUTIME_ID,&ts);return ts.tv_sec*1000.0+ts.tv_nsec/1e6;}
int main(){
 std::string url,etag;std::getline(std::cin,url);std::getline(std::cin,etag);uint64_t total=0,count=0;std::cin>>total>>count;
 if(url.rfind("http://172.17.",0)!=0 || count==0 || count>20000 || total==0)return 2;
 std::vector<std::pair<uint64_t,uint64_t>> ranges;for(uint64_t i=0,a,b;i<count;++i){std::cin>>a>>b;if(!std::cin || a>b || b>=total)return 3;ranges.emplace_back(a,b);}
 if(curl_global_init(CURL_GLOBAL_DEFAULT)!=CURLE_OK)return 4;
 CURL* curl=curl_easy_init();EVP_MD_CTX* digest=EVP_MD_CTX_new();if(!curl || !digest || EVP_DigestInit_ex(digest,EVP_sha256(),nullptr)!=1)return 5;
 State state{digest};state.etag=etag;state.total=total;curl_slist* headers=nullptr;headers=curl_slist_append(headers,("If-Match: "+etag).c_str());
 curl_easy_setopt(curl,CURLOPT_URL,url.c_str());curl_easy_setopt(curl,CURLOPT_HTTPHEADER,headers);
 curl_easy_setopt(curl,CURLOPT_AWS_SIGV4,"aws:amz:us-east-1:s3");curl_easy_setopt(curl,CURLOPT_USERPWD,"fixture:fixture");
 curl_easy_setopt(curl,CURLOPT_PROXY,"");curl_easy_setopt(curl,CURLOPT_NOSIGNAL,1L);curl_easy_setopt(curl,CURLOPT_TIMEOUT_MS,10000L);
 curl_easy_setopt(curl,CURLOPT_WRITEFUNCTION,body);curl_easy_setopt(curl,CURLOPT_WRITEDATA,&state);
 curl_easy_setopt(curl,CURLOPT_HEADERFUNCTION,header);curl_easy_setopt(curl,CURLOPT_HEADERDATA,&state);
 auto began=std::chrono::steady_clock::now();double cpu=cpu_ms();uint64_t bytes=0,connections=0;
 for(auto [a,b]:ranges){state.bytes=0;state.expected=b-a+1;state.start=a;state.end=b;state.range_ok=state.etag_ok=false;state.buffer.clear();state.buffer.reserve(state.expected);std::string range=std::to_string(a)+"-"+std::to_string(b);curl_easy_setopt(curl,CURLOPT_RANGE,range.c_str());
  auto code=curl_easy_perform(curl);long status=0,created=0;curl_easy_getinfo(curl,CURLINFO_RESPONSE_CODE,&status);curl_easy_getinfo(curl,CURLINFO_NUM_CONNECTS,&created);
  if(code!=CURLE_OK || status!=206 || state.bytes!=state.expected || !state.range_ok || !state.etag_ok){std::cerr<<"range replay failed; curl="<<int(code)<<" status="<<status<<"\n";return 6;}
  if(EVP_DigestUpdate(digest,state.buffer.data(),state.buffer.size())!=1)return 7;
  bytes+=state.bytes;connections+=created;
 }
 double elapsed=std::chrono::duration<double,std::milli>(std::chrono::steady_clock::now()-began).count();cpu=cpu_ms()-cpu;
 unsigned char hash[EVP_MAX_MD_SIZE];unsigned int len=0;EVP_DigestFinal_ex(digest,hash,&len);std::ostringstream hex;for(unsigned i=0;i<len;++i)hex<<std::hex<<std::setw(2)<<std::setfill('0')<<unsigned(hash[i]);
 std::cout<<"{\"passed\":true,\"wallMs\":"<<elapsed<<",\"processCpuMs\":"<<cpu<<",\"gets\":"<<count<<",\"bytes\":"<<bytes<<",\"connections\":"<<connections<<",\"sha256\":\""<<hex.str()<<"\"}\n";
 EVP_MD_CTX_free(digest);curl_slist_free_all(headers);curl_easy_cleanup(curl);curl_global_cleanup();
}
