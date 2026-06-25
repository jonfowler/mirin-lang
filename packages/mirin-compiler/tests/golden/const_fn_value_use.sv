module shifter #(parameter int N) (
    input  logic [15:0] x,
    output logic [15:0] result
);
    function automatic int count_pairs(input int n);
        int acc;
        acc = 0;
        for (int i = 0; i < n; i++) begin
            acc = (acc + (i % 2));
        end
        return acc;
    endfunction
    localparam int w = count_pairs(N);
    assign result = (x << w);
endmodule
